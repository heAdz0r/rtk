// E0.1: Indexing/scanning extracted from mod.rs
use anyhow::{Context, Result};
use ignore::WalkBuilder;
use std::collections::{BTreeMap, HashMap, HashSet}; // E3.2: HashSet for cascade sets
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use super::cache::{
    canonical_project_root, epoch_secs, format_hash, hash_file, is_artifact_stale, load_artifact,
    project_cache_key,
};
use super::extractor;
use super::manifest;
use super::{
    BuildState, DeltaKind, DeltaSummary, FileArtifact, FileDelta, FileMeta, GraphSummary,
    ProjectArtifact, ScanStats, ARTIFACT_VERSION, EXCLUDED_DIRS,
};

pub(super) fn build_state(
    project: &Path,
    refresh: bool,
    cascade_enabled: bool, // E6.4: feature flag — skip cascade invalidation when false
    verbose: u8,
) -> Result<BuildState> {
    let project_root = canonical_project_root(project)?;
    let project_id = project_cache_key(&project_root);

    let previous = load_artifact(&project_root)?;
    let previous_exists = previous.is_some();
    let stale_previous = previous.as_ref().map(is_artifact_stale).unwrap_or(false);

    let previous_map: HashMap<String, FileArtifact> = previous
        .as_ref()
        .map(|artifact| {
            artifact
                .files
                .iter()
                .cloned()
                .map(|entry| (entry.rel_path.clone(), entry))
                .collect()
        })
        .unwrap_or_default();

    let current_files = scan_project_metadata(&project_root)?;
    let force_rehash = refresh || stale_previous;
    let (files, delta, scan_stats, total_bytes) = build_incremental_files(
        &current_files,
        &previous_map,
        force_rehash,
        cascade_enabled,
        verbose,
    )?; // E6.4: pass cascade flag

    let now = epoch_secs(SystemTime::now());
    let created_at = previous.as_ref().map(|a| a.created_at).unwrap_or(now);

    let dep_manifest = manifest::parse_dep_manifest(&project_root); // L4: parse fresh on every build
    let artifact = ProjectArtifact {
        version: ARTIFACT_VERSION,
        project_id: project_id.clone(),
        project_root: project_root.to_string_lossy().to_string(),
        created_at,
        updated_at: now,
        file_count: files.len(),
        total_bytes,
        files,
        dep_manifest, // L4: dependency manifest
    };

    let cache_hit = previous_exists && !refresh && !stale_previous && delta.changes.is_empty();
    let graph = summarize_graph(&artifact);

    Ok(BuildState {
        project_root,
        project_id,
        previous_exists,
        stale_previous,
        cache_hit,
        scan_stats,
        artifact,
        delta,
        graph,
    })
}

pub(super) fn artifact_is_dirty(project_root: &Path, artifact: &ProjectArtifact) -> Result<bool> {
    let current_files = scan_project_metadata(project_root)?;
    if current_files.len() != artifact.files.len() {
        return Ok(true);
    }

    let previous_map: HashMap<&str, &FileArtifact> = artifact
        .files
        .iter()
        .map(|entry| (entry.rel_path.as_str(), entry))
        .collect();

    for (rel_path, meta) in current_files {
        let Some(previous) = previous_map.get(rel_path.as_str()) else {
            return Ok(true);
        };

        if previous.size != meta.size || previous.mtime_ns != meta.mtime_ns {
            return Ok(true);
        }
    }

    Ok(false)
}

pub(super) fn build_git_delta(
    project_root: &Path,
    since_rev: &str,
    verbose: u8,
) -> Result<DeltaSummary> {
    let output = Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["diff", "--name-status", "--find-renames"])
        .arg(format!("{since_rev}..HEAD"))
        .arg("--")
        .output()
        .with_context(|| {
            format!(
                "Failed to run git diff for delta since '{}' in {}",
                since_rev,
                project_root.display()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "git diff failed for --since '{}': {}",
            since_rev,
            stderr.trim()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut changes: Vec<FileDelta> = Vec::new();

    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 2 {
            continue;
        }

        let status = fields[0];
        let (change, rel_path) = if status.starts_with('R') {
            if fields.len() < 3 {
                continue;
            }
            (DeltaKind::Modified, fields[2])
        } else if status.starts_with('A') {
            (DeltaKind::Added, fields[1])
        } else if status.starts_with('M') {
            (DeltaKind::Modified, fields[1])
        } else if status.starts_with('D') {
            (DeltaKind::Removed, fields[1])
        } else {
            continue;
        };

        let rel = normalize_rel_path(Path::new(rel_path));
        if should_skip_rel_path(Path::new(&rel)) {
            continue;
        }

        let (old_hash, new_hash) = hashes_for_git_delta(project_root, &rel, change);
        changes.push(FileDelta {
            path: rel,
            change,
            old_hash,
            new_hash,
        });
    }

    changes.sort_by(|a, b| a.path.cmp(&b.path));
    let delta = summarize_delta(changes);

    if verbose > 0 {
        eprintln!(
            "memory.delta since={} +{} ~{} -{}",
            since_rev, delta.added, delta.modified, delta.removed
        );
    }

    Ok(delta)
}

fn hashes_for_git_delta(
    project_root: &Path,
    rel_path: &str,
    change: DeltaKind,
) -> (Option<String>, Option<String>) {
    if matches!(change, DeltaKind::Removed) {
        return (None, None);
    }

    let abs = project_root.join(PathBuf::from(rel_path));
    let hash = hash_file(&abs).ok().map(format_hash);
    (None, hash)
}

/// E3.2: Compute all module-name stems for a file path for import-graph matching.
/// E.g. "src/memory_layer/cache.rs" → ["cache", "cache.rs",
///      "src/memory_layer/cache", "src::memory_layer::cache",
///      "crate::src::memory_layer::cache", "./cache"]
fn module_stems_for_path(rel_path: &str) -> Vec<String> {
    let p = Path::new(rel_path);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(rel_path);
    let name_with_ext = p.file_name().and_then(|s| s.to_str()).unwrap_or(rel_path);

    let extensions = [
        ".rs", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".py", ".go",
    ];
    let mut no_ext = rel_path;
    for ext in &extensions {
        if let Some(stripped) = rel_path.strip_suffix(ext) {
            no_ext = stripped;
            break;
        }
    }

    let double_colon = no_ext.replace(['/', '\\'], "::");
    let mut stems = vec![
        stem.to_string(),
        name_with_ext.to_string(),
        no_ext.to_string(),   // "src/memory_layer/cache"
        double_colon.clone(), // "src::memory_layer::cache"
        format!("./{stem}"),  // relative TS/JS import
    ];
    // Rust: add crate:: prefix variant
    if !double_colon.starts_with("crate::") {
        stems.push(format!("crate::{double_colon}"));
    }
    stems
}

/// E3.2: Find files in `previous_map` that import any of the `changed_paths`.
/// Uses suffix-matching of module stems against import strings (language-agnostic heuristic).
fn find_cascade_dependents(
    changed_paths: &HashSet<String>,
    previous_map: &HashMap<String, FileArtifact>,
) -> HashSet<String> {
    let changed_stems: Vec<String> = changed_paths
        .iter()
        .flat_map(|p| module_stems_for_path(p))
        .collect();

    if changed_stems.is_empty() {
        return HashSet::new();
    }

    let mut dependents = HashSet::new();
    'file_loop: for (rel_path, artifact) in previous_map {
        if changed_paths.contains(rel_path) {
            continue; // already being rehashed
        }
        for import in &artifact.imports {
            if import.starts_with("self:") {
                continue; // skip synthetic anchors
            }
            for stem in &changed_stems {
                if import.contains(stem.as_str()) {
                    dependents.insert(rel_path.clone());
                    continue 'file_loop;
                }
            }
        }
    }
    dependents
}

fn build_incremental_files(
    current_files: &BTreeMap<String, FileMeta>,
    previous_map: &HashMap<String, FileArtifact>,
    force_rehash: bool,
    cascade_enabled: bool, // E6.4: feature flag — when false, skip import-graph cascade pass
    verbose: u8,
) -> Result<(Vec<FileArtifact>, DeltaSummary, ScanStats, u64)> {
    // E3.2: Pre-pass — identify metadata-changed files, then expand via import graph cascade.
    let cascade_paths: HashSet<String> = if cascade_enabled
        && !force_rehash
        && !previous_map.is_empty()
    {
        // E6.4: guard
        let metadata_changed: HashSet<String> = current_files
            .iter()
            .filter_map(|(rel_path, meta)| {
                match previous_map.get(rel_path) {
                    Some(prev) if prev.size == meta.size && prev.mtime_ns == meta.mtime_ns => None,
                    _ => Some(rel_path.clone()), // modified or new file
                }
            })
            .collect();
        let deps = find_cascade_dependents(&metadata_changed, previous_map);
        if verbose > 0 && !deps.is_empty() {
            eprintln!(
                "memory.cascade: {} dependent files queued for rehash (from {} changed)",
                deps.len(),
                metadata_changed.len()
            );
        }
        deps
    } else {
        HashSet::new()
    };

    let mut files: Vec<FileArtifact> = Vec::with_capacity(current_files.len());
    let mut changes: Vec<FileDelta> = Vec::new();
    let mut scan_stats = ScanStats {
        scanned_files: current_files.len(),
        ..ScanStats::default()
    };
    let mut total_bytes: u64 = 0;

    for (rel_path, meta) in current_files {
        total_bytes = total_bytes.saturating_add(meta.size);

        match previous_map.get(rel_path) {
            Some(previous) => {
                let metadata_match =
                    previous.size == meta.size && previous.mtime_ns == meta.mtime_ns;
                // E3.2: also force rehash if this file is a cascade dependent
                if metadata_match && !force_rehash && !cascade_paths.contains(rel_path) {
                    scan_stats.reused_entries += 1;
                    files.push(previous.clone());
                    continue;
                }

                let current_hash = hash_file(&meta.abs_path)
                    .with_context(|| format!("Failed to hash {}", meta.abs_path.display()))?;
                scan_stats.rehashed_entries += 1;

                if current_hash == previous.hash {
                    let mut kept = previous.clone();
                    kept.size = meta.size;
                    kept.mtime_ns = meta.mtime_ns;
                    files.push(kept);
                    continue;
                }

                let mut next = previous.clone();
                let analysis = extractor::analyze_file(&meta.abs_path, meta.size, current_hash)?;
                next.size = meta.size;
                next.mtime_ns = meta.mtime_ns;
                next.hash = current_hash;
                next.language = analysis.language;
                next.line_count = analysis.line_count;
                next.imports = analysis.imports;
                next.pub_symbols = analysis.pub_symbols; // L3: refresh API surface
                next.type_relations = analysis.type_relations; // L2: refresh type graph
                files.push(next);

                changes.push(FileDelta {
                    path: rel_path.clone(),
                    change: DeltaKind::Modified,
                    old_hash: Some(format_hash(previous.hash)),
                    new_hash: Some(format_hash(current_hash)),
                });
            }
            None => {
                let current_hash = hash_file(&meta.abs_path).with_context(|| {
                    format!(
                        "Failed to hash newly discovered file {}",
                        meta.abs_path.display()
                    )
                })?;
                scan_stats.rehashed_entries += 1;

                let analysis = extractor::analyze_file(&meta.abs_path, meta.size, current_hash)?;
                files.push(FileArtifact {
                    rel_path: rel_path.clone(),
                    size: meta.size,
                    mtime_ns: meta.mtime_ns,
                    hash: current_hash,
                    language: analysis.language,
                    line_count: analysis.line_count,
                    imports: analysis.imports,
                    pub_symbols: analysis.pub_symbols, // L3: public API surface
                    type_relations: analysis.type_relations, // L2: type graph edges
                });

                changes.push(FileDelta {
                    path: rel_path.clone(),
                    change: DeltaKind::Added,
                    old_hash: None,
                    new_hash: Some(format_hash(current_hash)),
                });
            }
        }
    }

    for (rel_path, previous) in previous_map {
        if !current_files.contains_key(rel_path) {
            changes.push(FileDelta {
                path: rel_path.clone(),
                change: DeltaKind::Removed,
                old_hash: Some(format_hash(previous.hash)),
                new_hash: None,
            });
        }
    }

    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    changes.sort_by(|a, b| a.path.cmp(&b.path));

    if verbose > 0 {
        eprintln!(
            "memory.index files={} reused={} rehashed={} delta={} (+{} ~{} -{})",
            current_files.len(),
            scan_stats.reused_entries,
            scan_stats.rehashed_entries,
            changes.len(),
            changes
                .iter()
                .filter(|c| matches!(c.change, DeltaKind::Added))
                .count(),
            changes
                .iter()
                .filter(|c| matches!(c.change, DeltaKind::Modified))
                .count(),
            changes
                .iter()
                .filter(|c| matches!(c.change, DeltaKind::Removed))
                .count(),
        );
    }

    let delta = summarize_delta(changes);
    Ok((files, delta, scan_stats, total_bytes))
}

pub(super) fn scan_project_metadata(project_root: &Path) -> Result<BTreeMap<String, FileMeta>> {
    let mut result: BTreeMap<String, FileMeta> = BTreeMap::new();

    let mut builder = WalkBuilder::new(project_root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .follow_links(false);

    for entry in builder.build() {
        let entry = match entry {
            Ok(value) => value,
            Err(_) => continue,
        };

        let file_type = match entry.file_type() {
            Some(file_type) => file_type,
            None => continue,
        };

        if !file_type.is_file() {
            continue;
        }

        let abs_path = entry.path();
        let rel_path = match abs_path.strip_prefix(project_root) {
            Ok(rel) => rel,
            Err(_) => continue,
        };

        if should_skip_rel_path(rel_path) {
            continue;
        }

        let metadata = match fs::metadata(abs_path) {
            Ok(m) => m,
            Err(_) => continue,
        };

        let rel = normalize_rel_path(rel_path);
        let mtime_ns = metadata
            .modified()
            .ok()
            .and_then(|mtime| mtime.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0);

        result.insert(
            rel,
            FileMeta {
                abs_path: abs_path.to_path_buf(),
                size: metadata.len(),
                mtime_ns,
            },
        );
    }

    Ok(result)
}

pub(super) fn should_skip_rel_path(path: &Path) -> bool {
    if path
        .components()
        .any(|component| EXCLUDED_DIRS.contains(&component.as_os_str().to_string_lossy().as_ref()))
    {
        return true;
    }

    let rel = normalize_rel_path(path);
    rel.ends_with(".rtk-lock")
}

/// Returns true if a FS event on `abs_path` should trigger re-indexing. // E3.1
/// Skips paths inside excluded dirs (target, node_modules, .git, etc.) and .rtk-lock files.
pub(super) fn should_watch_abs_path(project_root: &Path, abs_path: &Path) -> bool {
    abs_path
        .strip_prefix(project_root)
        .map(|rel| !should_skip_rel_path(rel))
        .unwrap_or(false) // outside project root — ignore
}

fn normalize_rel_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub(super) fn summarize_delta(changes: Vec<FileDelta>) -> DeltaSummary {
    let added = changes
        .iter()
        .filter(|item| matches!(item.change, DeltaKind::Added))
        .count();
    let modified = changes
        .iter()
        .filter(|item| matches!(item.change, DeltaKind::Modified))
        .count();
    let removed = changes
        .iter()
        .filter(|item| matches!(item.change, DeltaKind::Removed))
        .count();

    DeltaSummary {
        added,
        modified,
        removed,
        changes,
    }
}

pub(super) fn summarize_graph(artifact: &ProjectArtifact) -> GraphSummary {
    let edges = artifact
        .files
        .iter()
        .map(|file| file.imports.len())
        .sum::<usize>();

    GraphSummary {
        nodes: artifact.files.len(),
        edges,
    }
}

#[cfg(test)]
mod tests {
    use super::super::cache::project_cache_key;
    use super::super::{FileArtifact, ProjectArtifact, ARTIFACT_VERSION};
    use super::*;
    use tempfile::TempDir;

    fn make_artifact_for_dir(dir: &std::path::Path) -> ProjectArtifact {
        let current_files = scan_project_metadata(dir).expect("scan");
        let project_id = project_cache_key(dir);
        let mut files: Vec<FileArtifact> = current_files
            .iter()
            .map(|(rel, meta)| FileArtifact {
                rel_path: rel.clone(),
                size: meta.size,
                mtime_ns: meta.mtime_ns,
                hash: 0,
                language: None,
                line_count: None,
                imports: vec![],
                pub_symbols: vec![],
                type_relations: vec![],
            })
            .collect();
        files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        ProjectArtifact {
            version: ARTIFACT_VERSION,
            project_id,
            project_root: dir.to_string_lossy().to_string(),
            created_at: 0,
            updated_at: 0,
            file_count: files.len(),
            total_bytes: files.iter().map(|f| f.size).sum(),
            files,
            dep_manifest: None,
        }
    }

    #[test]
    fn artifact_is_dirty_detects_mtime_change() {
        let dir = TempDir::new().unwrap();
        let src_file = dir.path().join("main.rs");
        std::fs::write(&src_file, "fn main() {}").unwrap();

        let artifact = make_artifact_for_dir(dir.path());
        // Initially clean
        assert!(!artifact_is_dirty(dir.path(), &artifact).unwrap());

        // Ensure measurable mtime delta
        std::thread::sleep(std::time::Duration::from_millis(20));
        // Modify file content → triggers mtime change
        std::fs::write(&src_file, "fn main() { println!(\"changed\"); }").unwrap();

        assert!(artifact_is_dirty(dir.path(), &artifact).unwrap());
    }

    #[test]
    fn artifact_is_dirty_clean_state() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn add(a: i32, b: i32) -> i32 { a + b }",
        )
        .unwrap();

        let artifact = make_artifact_for_dir(dir.path());
        assert!(
            !artifact_is_dirty(dir.path(), &artifact).unwrap(),
            "freshly scanned artifact should not be dirty"
        );
    }

    #[test]
    fn build_git_delta_fails_without_git_repo() {
        let dir = TempDir::new().unwrap();
        // tmpdir has no .git — git command must fail
        let result = build_git_delta(dir.path(), "HEAD~3", 0);
        assert!(result.is_err(), "should fail: no git repo in tmpdir");
    }

    // ── E3.2 cascade tests ───────────────────────────────────────────────────

    #[test]
    fn module_stems_for_rust_path() {
        // E3.2: stem generation must include all common Rust import variants
        let stems = module_stems_for_path("src/memory_layer/cache.rs");
        assert!(stems.contains(&"cache".to_string()));
        assert!(stems.contains(&"cache.rs".to_string()));
        assert!(stems.contains(&"src/memory_layer/cache".to_string()));
        assert!(stems.contains(&"src::memory_layer::cache".to_string()));
        assert!(stems.contains(&"crate::src::memory_layer::cache".to_string()));
    }

    #[test]
    fn find_cascade_dependents_finds_direct_importer() {
        // E3.2: file B imports module A; when A changes, B must be in cascade set
        let mut previous_map: HashMap<String, FileArtifact> = HashMap::new();

        let file_a = FileArtifact {
            rel_path: "src/cache.rs".to_string(),
            size: 100,
            mtime_ns: 1,
            hash: 1,
            language: Some("rust".to_string()),
            line_count: Some(10),
            imports: vec![],
            pub_symbols: vec![],
            type_relations: vec![],
        };
        let file_b = FileArtifact {
            rel_path: "src/main.rs".to_string(),
            size: 200,
            mtime_ns: 2,
            hash: 2,
            language: Some("rust".to_string()),
            line_count: Some(20),
            // main.rs imports cache module (as recorded by extract_imports)
            imports: vec!["crate::cache".to_string()],
            pub_symbols: vec![],
            type_relations: vec![],
        };
        previous_map.insert(file_a.rel_path.clone(), file_a);
        previous_map.insert(file_b.rel_path.clone(), file_b);

        let mut changed = HashSet::new();
        changed.insert("src/cache.rs".to_string()); // A changed

        let deps = find_cascade_dependents(&changed, &previous_map);
        assert!(
            deps.contains("src/main.rs"),
            "main.rs imports cache, should be in cascade set; got: {:?}",
            deps
        );
        assert!(
            !deps.contains("src/cache.rs"),
            "changed file itself must not be in cascade set"
        );
    }

    #[test]
    fn find_cascade_dependents_empty_on_no_match() {
        // E3.2: no file imports the changed file → cascade set is empty
        let mut previous_map: HashMap<String, FileArtifact> = HashMap::new();
        previous_map.insert(
            "src/util.rs".to_string(),
            FileArtifact {
                rel_path: "src/util.rs".to_string(),
                size: 50,
                mtime_ns: 1,
                hash: 1,
                language: Some("rust".to_string()),
                line_count: Some(5),
                imports: vec!["std::collections::HashMap".to_string()],
                pub_symbols: vec![],
                type_relations: vec![],
            },
        );

        let mut changed = HashSet::new();
        changed.insert("src/unrelated.rs".to_string());

        let deps = find_cascade_dependents(&changed, &previous_map);
        assert!(
            deps.is_empty(),
            "no file imports unrelated.rs; expected empty set"
        );
    }
}

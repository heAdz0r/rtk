use crate::filter::Language;
use crate::read_symbols::{SymbolExtractor, Visibility};
use crate::symbols_regex::RegexExtractor;
use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use ignore::WalkBuilder;
use lazy_static::lazy_static;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use xxhash_rust::xxh3::{xxh3_64, Xxh3}; // L3: public API surface extraction

const ARTIFACT_VERSION: u32 = 3; // bumped: added dep_manifest (L4) and QueryType
const CACHE_MAX_PROJECTS: usize = 64;
const CACHE_TTL_SECS: u64 = 24 * 60 * 60;
const IMPORT_SCAN_MAX_BYTES: u64 = 512 * 1024;
const MAX_SYMBOLS_PER_FILE: usize = 64; // L3: cap symbols per file

const EXCLUDED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".cache",
    ".turbo",
    ".venv",
    "venv",
    "__pycache__",
    "coverage",
];

const ENTRY_POINT_HINTS: &[&str] = &[
    "README.md",
    "Cargo.toml",
    "src/main.rs",
    "src/lib.rs",
    "package.json",
    "tsconfig.json",
    "pyproject.toml",
    "go.mod",
    "main.py",
    "src/index.ts",
    "src/main.ts",
    "src/index.js",
    "src/main.js",
];

/// Compact representation of a public symbol for the L3 api_surface layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SymbolSummary {
    kind: String, // fn | struct | enum | trait | type | const | class | iface | mod
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    sig: Option<String>, // one-line signature for functions
}

/// Public API surface of a single file (L3 artifact layer).
#[derive(Debug, Serialize)]
struct FileSurface {
    path: String,
    lang: String,
    symbols: Vec<SymbolSummary>,
}

/// L1: Module-level export summary (names only, no signatures).
#[derive(Debug, Serialize)]
struct ModuleIndexEntry {
    module: String,
    lang: String,
    exports: Vec<String>,
}

/// L4: A single dependency entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DepEntry {
    name: String,
    version: String,
}

/// L4: Dependency manifest parsed from Cargo.toml / package.json / pyproject.toml.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct DepManifest {
    runtime: Vec<DepEntry>,
    dev: Vec<DepEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    build: Vec<DepEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum DetailLevel {
    Compact,
    Normal,
    Verbose,
}

/// Query-type hint that drives relevance-layer filtering (E2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum, Default)]
#[serde(rename_all = "lowercase")]
pub enum QueryType {
    /// Return all layers (default)
    #[default]
    General,
    /// L1 (module_index) + L3 (api_surface) + L6 (delta)
    Bugfix,
    /// L0 (project_map) + L1 + L3 + L4 (dep_manifest)
    Feature,
    /// L1 + L3 — module structure and API surface only
    Refactor,
    /// L3 + L4 + L6 — API surface, deps, and recent changes
    Incident,
}

#[derive(Debug, Clone, Copy)]
struct DetailLimits {
    max_changes: usize,
    max_entry_points: usize,
    max_hot_paths: usize,
    max_imports: usize,
    max_api_files: usize,      // L3: files to show in api_surface
    max_api_symbols: usize,    // L3: symbols per file
    max_modules: usize,        // L1: max modules in module_index
    max_module_exports: usize, // L1: max exports per module
}

/// Controls which artifact layers appear in the context output (E2.3).
#[derive(Debug, Clone, Copy)]
struct LayerFlags {
    l0_project_map: bool,   // entry_points + hot_paths
    l1_module_index: bool,  // module export summary
    l3_api_surface: bool,   // public API with signatures
    l4_dep_manifest: bool,  // dependency manifest
    l6_change_digest: bool, // delta / recent changes
    top_imports: bool,      // top imported modules
}

#[derive(Debug, Clone)]
struct BuildState {
    project_root: PathBuf,
    project_id: String,
    previous_exists: bool,
    stale_previous: bool,
    cache_hit: bool,
    scan_stats: ScanStats,
    artifact: ProjectArtifact,
    delta: DeltaSummary,
    graph: GraphSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CacheStatus {
    Hit,
    Miss,
    Refreshed,
    StaleRebuild,
}

#[derive(Debug, Serialize)]
struct MemoryResponse {
    command: String,
    project_root: String,
    project_id: String,
    artifact_version: u32,
    detail: DetailLevel,
    cache_status: CacheStatus,
    cache_hit: bool,
    stats: ProjectStats,
    delta: DeltaPayload,
    context: ContextSlice,
    graph: GraphSummary,
}

#[derive(Debug, Serialize)]
struct ProjectStats {
    file_count: usize,
    total_bytes: u64,
    reused_entries: usize,
    rehashed_entries: usize,
    scanned_files: usize,
}

#[derive(Debug, Serialize)]
struct DeltaPayload {
    added: usize,
    modified: usize,
    removed: usize,
    files: Vec<FileDelta>,
}

#[derive(Debug, Serialize)]
struct ContextSlice {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    entry_points: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    hot_paths: Vec<PathStat>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    top_imports: Vec<ImportStat>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    api_surface: Vec<FileSurface>, // L3: public API surface
    #[serde(skip_serializing_if = "Vec::is_empty")]
    module_index: Vec<ModuleIndexEntry>, // L1: module export summary
    #[serde(skip_serializing_if = "Option::is_none")]
    dep_manifest: Option<DepManifest>, // L4: dependency manifest
}

#[derive(Debug, Clone, Serialize)]
struct PathStat {
    path: String,
    count: usize,
}

#[derive(Debug, Clone, Serialize)]
struct ImportStat {
    module: String,
    count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectArtifact {
    version: u32,
    project_id: String,
    project_root: String,
    created_at: u64,
    updated_at: u64,
    file_count: usize,
    total_bytes: u64,
    files: Vec<FileArtifact>,
    #[serde(default)]
    dep_manifest: Option<DepManifest>, // L4: cached dependency manifest
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileArtifact {
    rel_path: String,
    size: u64,
    mtime_ns: u64,
    hash: u64,
    language: Option<String>,
    line_count: Option<u32>,
    imports: Vec<String>,
    #[serde(default)]
    pub_symbols: Vec<SymbolSummary>, // L3: cached public API surface
}

#[derive(Debug, Clone)]
struct FileMeta {
    abs_path: PathBuf,
    size: u64,
    mtime_ns: u64,
}

#[derive(Debug, Clone, Default)]
struct ScanStats {
    scanned_files: usize,
    reused_entries: usize,
    rehashed_entries: usize,
}

#[derive(Debug, Clone, Serialize)]
struct DeltaSummary {
    added: usize,
    modified: usize,
    removed: usize,
    changes: Vec<FileDelta>,
}

#[derive(Debug, Clone, Serialize)]
struct FileDelta {
    path: String,
    change: DeltaKind,
    old_hash: Option<String>,
    new_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum DeltaKind {
    Added,
    Modified,
    Removed,
}

#[derive(Debug, Clone, Serialize)]
struct GraphSummary {
    nodes: usize,
    edges: usize,
}

pub fn run_explore(
    project: &Path,
    refresh: bool,
    detail: DetailLevel,
    format: &str,
    query_type: QueryType, // E2.3: relevance-layer filtering
    verbose: u8,
) -> Result<()> {
    let state = build_state(project, refresh, verbose)?;
    let should_store = refresh || !state.cache_hit;
    if should_store {
        store_artifact(&state.artifact)?;
    }

    let response = build_response("explore", &state, detail, refresh, query_type);
    print_response(&response, format)
}

pub fn run_delta(
    project: &Path,
    detail: DetailLevel,
    format: &str,
    query_type: QueryType, // E2.3
    verbose: u8,
) -> Result<()> {
    let state = build_state(project, false, verbose)?;
    if !state.delta.changes.is_empty() {
        store_artifact(&state.artifact)?;
    }

    let response = build_response("delta", &state, detail, false, query_type);
    print_response(&response, format)
}

pub fn run_refresh(
    project: &Path,
    detail: DetailLevel,
    format: &str,
    query_type: QueryType, // E2.3
    verbose: u8,
) -> Result<()> {
    let state = build_state(project, true, verbose)?;
    store_artifact(&state.artifact)?;

    let response = build_response("refresh", &state, detail, true, query_type);
    print_response(&response, format)
}

pub fn run_watch(
    project: &Path,
    interval_secs: u64,
    detail: DetailLevel,
    format: &str,
    query_type: QueryType, // E2.3
    verbose: u8,
) -> Result<()> {
    let interval = Duration::from_secs(interval_secs.max(1));

    loop {
        let state = build_state(project, false, verbose)?;
        if !state.delta.changes.is_empty() || state.stale_previous || !state.previous_exists {
            store_artifact(&state.artifact)?;
            let response = build_response("watch", &state, detail, false, query_type);
            print_response(&response, format)?;
        } else if verbose > 0 {
            eprintln!(
                "memory.watch project={} clean",
                state.project_root.to_string_lossy()
            );
        }

        std::thread::sleep(interval);
    }
}

/// Install (or uninstall) the rtk-mem-context.sh PreToolUse:Task hook in ~/.claude/settings.json
pub fn run_install_hook(uninstall: bool, status_only: bool, verbose: u8) -> Result<()> {
    use std::collections::BTreeMap;

    let settings_path = dirs::home_dir()
        .context("Cannot find home directory")?
        .join(".claude")
        .join("settings.json");

    // Read existing settings (or start with empty object)
    let raw = if settings_path.exists() {
        fs::read_to_string(&settings_path).context("Failed to read settings.json")?
    } else {
        "{}".to_string()
    };

    let mut settings: serde_json::Value =
        serde_json::from_str(&raw).context("Failed to parse settings.json")?;

    // Locate the hook binary next to the installed rtk binary
    let hook_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("rtk-mem-context.sh")))
        .or_else(|| {
            // Fall back to hooks/ in the RTK source tree
            std::env::var("RTK_HOOKS_DIR")
                .ok()
                .map(|d| std::path::PathBuf::from(d).join("rtk-mem-context.sh"))
        })
        .unwrap_or_else(|| std::path::PathBuf::from("hooks/rtk-mem-context.sh"));

    let hook_entry = serde_json::json!({
        "matcher": "Task",
        "hooks": [{
            "type": "command",
            "command": hook_bin.to_string_lossy().to_string(),
            "timeout": 10
        }]
    });

    // Read current PreToolUse array
    let pre = settings
        .get("hooks")
        .and_then(|h| h.get("PreToolUse"))
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();

    let already_installed = pre.iter().any(|entry| {
        entry.get("matcher").and_then(|m| m.as_str()) == Some("Task")
            && entry
                .get("hooks")
                .and_then(|h| h.as_array())
                .map(|hooks| {
                    hooks.iter().any(|h| {
                        h.get("command")
                            .and_then(|c| c.as_str())
                            .map(|c| c.contains("rtk-mem-context"))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
    });

    if status_only {
        println!(
            "memory.hook status={} path={}",
            if already_installed {
                "installed"
            } else {
                "not_installed"
            },
            settings_path.display()
        );
        return Ok(());
    }

    if uninstall {
        if !already_installed {
            println!("memory.hook uninstall: nothing to remove");
            return Ok(());
        }
        // Remove Task/rtk-mem-context entries from PreToolUse
        let filtered: Vec<serde_json::Value> = pre
            .into_iter()
            .filter(|entry| {
                let is_task = entry.get("matcher").and_then(|m| m.as_str()) == Some("Task");
                let has_mem_hook = entry
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .map(|hooks| {
                        hooks.iter().any(|h| {
                            h.get("command")
                                .and_then(|c| c.as_str())
                                .map(|c| c.contains("rtk-mem-context"))
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false);
                !(is_task && has_mem_hook)
            })
            .collect();
        settings["hooks"]["PreToolUse"] = serde_json::json!(filtered);
        let json = serde_json::to_string_pretty(&settings)?;
        fs::write(&settings_path, json)?;
        println!("memory.hook uninstall ok path={}", settings_path.display());
        return Ok(());
    }

    if already_installed {
        println!(
            "memory.hook already installed path={}",
            settings_path.display()
        );
        if verbose > 0 {
            println!("  hook_bin: {}", hook_bin.display());
        }
        return Ok(());
    }

    // Add the hook entry
    let mut new_pre = pre;
    new_pre.push(hook_entry);
    settings["hooks"]["PreToolUse"] = serde_json::json!(new_pre);

    // Ensure parent directory exists
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(&settings)?;
    fs::write(&settings_path, &json)
        .with_context(|| format!("Failed to write {}", settings_path.display()))?;

    println!("memory.hook installed ok path={}", settings_path.display());
    if verbose > 0 {
        println!("  hook_bin: {}", hook_bin.display());
        println!("  fires on: PreToolUse:Task subagent_type=Explore");
    }
    Ok(())
}

pub fn run_status(project: &Path, verbose: u8) -> Result<()> {
    let project_root = canonical_project_root(project)?;
    let artifact = load_artifact(&project_root)?;
    match artifact {
        None => {
            println!(
                "memory.status project={} cache=miss",
                project_root.display()
            );
        }
        Some(a) => {
            let stale = is_artifact_stale(&a);
            let age_secs = epoch_secs(SystemTime::now()).saturating_sub(a.updated_at);
            println!(
                "memory.status project={} id={} cache={} files={} bytes={} updated={}s ago",
                project_root.display(),
                a.project_id,
                if stale { "stale" } else { "fresh" },
                a.file_count,
                format_bytes(a.total_bytes),
                age_secs
            );
            if verbose > 0 {
                let path = artifact_path(&project_root);
                println!("  path: {}", path.display());
                println!("  version: {}", a.version);
            }
        }
    }
    Ok(())
}

pub fn run_clear(project: &Path, _verbose: u8) -> Result<()> {
    let project_root = canonical_project_root(project)?;
    let path = artifact_path(&project_root);
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("Failed to remove artifact {}", path.display()))?;
        println!("memory.clear project={} ok", project_root.display());
    } else {
        println!(
            "memory.clear project={} nothing to clear",
            project_root.display()
        );
    }
    Ok(())
}

fn build_state(project: &Path, refresh: bool, verbose: u8) -> Result<BuildState> {
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
    let (files, delta, scan_stats, total_bytes) =
        build_incremental_files(&current_files, &previous_map, refresh, verbose)?;

    let now = epoch_secs(SystemTime::now());
    let created_at = previous.as_ref().map(|a| a.created_at).unwrap_or(now);

    let dep_manifest = parse_dep_manifest(&project_root); // L4: parse fresh on every build
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

fn build_incremental_files(
    current_files: &BTreeMap<String, FileMeta>,
    previous_map: &HashMap<String, FileArtifact>,
    force_rehash: bool,
    verbose: u8,
) -> Result<(Vec<FileArtifact>, DeltaSummary, ScanStats, u64)> {
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
                if metadata_match && !force_rehash {
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
                let analysis = analyze_file(&meta.abs_path, meta.size, current_hash)?;
                next.size = meta.size;
                next.mtime_ns = meta.mtime_ns;
                next.hash = current_hash;
                next.language = analysis.language;
                next.line_count = analysis.line_count;
                next.imports = analysis.imports;
                next.pub_symbols = analysis.pub_symbols; // L3: refresh API surface
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

                let analysis = analyze_file(&meta.abs_path, meta.size, current_hash)?;
                files.push(FileArtifact {
                    rel_path: rel_path.clone(),
                    size: meta.size,
                    mtime_ns: meta.mtime_ns,
                    hash: current_hash,
                    language: analysis.language,
                    line_count: analysis.line_count,
                    imports: analysis.imports,
                    pub_symbols: analysis.pub_symbols, // L3: public API surface
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

fn scan_project_metadata(project_root: &Path) -> Result<BTreeMap<String, FileMeta>> {
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

fn should_skip_rel_path(path: &Path) -> bool {
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
fn should_watch_abs_path(project_root: &Path, abs_path: &Path) -> bool {
    abs_path
        .strip_prefix(project_root)
        .map(|rel| \!should_skip_rel_path(rel))
        .unwrap_or(false) // outside project root — ignore
}

fn normalize_rel_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

struct FileAnalysis {
    language: Option<String>,
    line_count: Option<u32>,
    imports: Vec<String>,
    pub_symbols: Vec<SymbolSummary>, // L3: public API surface
}

fn analyze_file(path: &Path, size: u64, current_hash: u64) -> Result<FileAnalysis> {
    let language = detect_language(path);

    if size > IMPORT_SCAN_MAX_BYTES {
        return Ok(FileAnalysis {
            language,
            line_count: None,
            imports: Vec::new(),
            pub_symbols: Vec::new(),
        });
    }

    let content = match fs::read_to_string(path) {
        Ok(value) => value,
        Err(_) => {
            return Ok(FileAnalysis {
                language,
                line_count: None,
                imports: Vec::new(),
                pub_symbols: Vec::new(),
            });
        }
    };

    let line_count = Some(content.lines().count() as u32);
    let mut imports = extract_imports(&content);
    imports.sort();
    imports.dedup();

    if imports.len() > 64 {
        imports.truncate(64);
    }

    // Include a synthetic hash anchor for downstream consumers.
    if imports.is_empty() {
        imports.push(format!("self:{:016x}", current_hash));
    }

    // L3: extract public symbols for API surface caching
    let pub_symbols = language
        .as_deref()
        .map(|lang| extract_file_symbols(&content, lang))
        .unwrap_or_default();

    Ok(FileAnalysis {
        language,
        line_count,
        imports,
        pub_symbols,
    })
}

fn language_str_to_filter(lang: &str) -> Option<Language> {
    match lang {
        "rust" => Some(Language::Rust),
        "typescript" => Some(Language::TypeScript),
        "javascript" => Some(Language::JavaScript),
        "python" => Some(Language::Python),
        "go" => Some(Language::Go),
        _ => None,
    }
}

fn symbol_kind_label(kind: crate::read_symbols::SymbolKind) -> &'static str {
    use crate::read_symbols::SymbolKind;
    match kind {
        SymbolKind::Function | SymbolKind::Method => "fn",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Trait => "trait",
        SymbolKind::Interface => "iface",
        SymbolKind::Class => "class",
        SymbolKind::Type => "type",
        SymbolKind::Constant => "const",
        SymbolKind::Module => "mod",
        SymbolKind::Import => "import",
    }
}

/// Extract public symbols from file content and return compact SymbolSummary list.
fn extract_file_symbols(content: &str, lang_str: &str) -> Vec<SymbolSummary> {
    let lang = match language_str_to_filter(lang_str) {
        Some(l) => l,
        None => return Vec::new(),
    };
    let extractor = RegexExtractor;
    extractor
        .extract(content, &lang)
        .into_iter()
        .filter(|s| s.visibility == Visibility::Public)
        .take(MAX_SYMBOLS_PER_FILE)
        .map(|s| SymbolSummary {
            kind: symbol_kind_label(s.kind).to_string(),
            name: s.name.clone(),
            sig: s.signature.clone(),
        })
        .collect()
}

fn detect_language(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let language = match ext.as_str() {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "rb" => "ruby",
        "php" => "php",
        "scala" => "scala",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => "cpp",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        _ => return None,
    };

    Some(language.to_string())
}

fn extract_imports(content: &str) -> Vec<String> {
    lazy_static! {
        static ref JS_IMPORT_RE: Regex =
            Regex::new(r#"^\s*import\s+.+\s+from\s+['\"]([^'\"]+)['\"]"#)
                .expect("valid JS import regex");
        static ref JS_REQUIRE_RE: Regex =
            Regex::new(r#"require\(\s*['\"]([^'\"]+)['\"]\s*\)"#).expect("valid JS require regex");
        static ref PY_IMPORT_RE: Regex =
            Regex::new(r"^\s*import\s+([A-Za-z0-9_\.]+)").expect("valid Python import regex");
        static ref PY_FROM_RE: Regex = Regex::new(r"^\s*from\s+([A-Za-z0-9_\.]+)\s+import\s+")
            .expect("valid Python from-import regex");
        static ref RUST_USE_RE: Regex =
            Regex::new(r"^\s*use\s+([^;]+);").expect("valid Rust use regex");
        static ref GO_IMPORT_RE: Regex =
            Regex::new(r#"^\s*import\s+['\"]([^'\"]+)['\"]"#).expect("valid Go import regex");
    }

    let mut imports = Vec::new();

    for line in content.lines() {
        if let Some(cap) = JS_IMPORT_RE.captures(line) {
            imports.push(cap[1].trim().to_string());
        }

        if let Some(cap) = JS_REQUIRE_RE.captures(line) {
            imports.push(cap[1].trim().to_string());
        }

        if let Some(cap) = PY_IMPORT_RE.captures(line) {
            imports.push(cap[1].trim().to_string());
        }

        if let Some(cap) = PY_FROM_RE.captures(line) {
            imports.push(cap[1].trim().to_string());
        }

        if let Some(cap) = RUST_USE_RE.captures(line) {
            imports.push(cap[1].trim().to_string());
        }

        if let Some(cap) = GO_IMPORT_RE.captures(line) {
            imports.push(cap[1].trim().to_string());
        }
    }

    imports
}

fn summarize_delta(changes: Vec<FileDelta>) -> DeltaSummary {
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

fn summarize_graph(artifact: &ProjectArtifact) -> GraphSummary {
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

fn build_response(
    command: &str,
    state: &BuildState,
    detail: DetailLevel,
    refresh: bool,
    query_type: QueryType, // E2.3
) -> MemoryResponse {
    let limits = limits_for_detail(detail);

    let cache_status = if refresh {
        CacheStatus::Refreshed
    } else if state.stale_previous {
        CacheStatus::StaleRebuild
    } else if state.cache_hit {
        CacheStatus::Hit
    } else {
        CacheStatus::Miss
    };

    let stats = ProjectStats {
        file_count: state.artifact.file_count,
        total_bytes: state.artifact.total_bytes,
        reused_entries: state.scan_stats.reused_entries,
        rehashed_entries: state.scan_stats.rehashed_entries,
        scanned_files: state.scan_stats.scanned_files,
    };

    let mut delta_files = state.delta.changes.clone();
    if delta_files.len() > limits.max_changes {
        delta_files.truncate(limits.max_changes);
    }

    let delta = DeltaPayload {
        added: state.delta.added,
        modified: state.delta.modified,
        removed: state.delta.removed,
        files: delta_files,
    };

    let context = build_context_slice(&state.artifact, &state.delta, limits, query_type); // E2.3

    MemoryResponse {
        command: command.to_string(),
        project_root: state.project_root.to_string_lossy().to_string(),
        project_id: state.project_id.clone(),
        artifact_version: state.artifact.version,
        detail,
        cache_status,
        cache_hit: state.cache_hit,
        stats,
        delta,
        context,
        graph: state.graph.clone(),
    }
}

fn build_context_slice(
    artifact: &ProjectArtifact,
    delta: &DeltaSummary,
    limits: DetailLimits,
    query_type: QueryType, // E2.3: relevance-layer filtering
) -> ContextSlice {
    let layers = layers_for(query_type); // E2.3: determine active layers

    // L0: project map (entry_points + hot_paths)
    let entry_points = if layers.l0_project_map {
        select_entry_points(&artifact.files, limits.max_entry_points)
    } else {
        Vec::new()
    };
    let hot_paths = if layers.l0_project_map {
        select_hot_paths(artifact, delta, limits.max_hot_paths)
    } else {
        Vec::new()
    };

    // top_imports
    let top_imports = if layers.top_imports {
        select_top_imports(&artifact.files, limits.max_imports)
    } else {
        Vec::new()
    };

    // L3: api_surface — show recently changed files or entry points
    // On initial indexing delta contains ALL files — fall back to entry_points.
    let api_surface = if layers.l3_api_surface {
        let use_delta =
            !delta.changes.is_empty() && delta.changes.len() <= limits.max_api_files * 4;
        let surface_paths: Vec<&str> = if use_delta {
            delta
                .changes
                .iter()
                .filter(|c| !matches!(c.change, DeltaKind::Removed))
                .map(|c| c.path.as_str())
                .collect()
        } else {
            entry_points.iter().map(|s| s.as_str()).collect()
        };
        build_api_surface(
            artifact,
            &surface_paths,
            limits.max_api_files,
            limits.max_api_symbols,
        )
    } else {
        Vec::new()
    };

    // L1: module_index — compact list of module exports
    let module_index = if layers.l1_module_index {
        build_module_index(artifact, limits.max_modules, limits.max_module_exports)
    } else {
        Vec::new()
    };

    // L4: dep_manifest — dependency manifest from cached artifact
    let dep_manifest = if layers.l4_dep_manifest {
        artifact.dep_manifest.clone()
    } else {
        None
    };

    ContextSlice {
        entry_points,
        hot_paths,
        top_imports,
        api_surface,
        module_index,
        dep_manifest,
    }
}

fn select_entry_points(files: &[FileArtifact], max: usize) -> Vec<String> {
    let mut picked: Vec<String> = ENTRY_POINT_HINTS
        .iter()
        .filter_map(|hint| {
            files
                .iter()
                .find(|file| file.rel_path == *hint || file.rel_path.ends_with(hint))
                .map(|file| file.rel_path.clone())
        })
        .collect();

    if picked.len() < max {
        for file in files {
            if picked.len() >= max {
                break;
            }
            if is_hidden_rel_path(&file.rel_path) {
                continue;
            }
            if file.rel_path.contains("main") || file.rel_path.contains("index") {
                if !picked.contains(&file.rel_path) {
                    picked.push(file.rel_path.clone());
                }
            }
        }
    }

    if picked.len() < max {
        for file in files {
            if picked.len() >= max {
                break;
            }
            if is_hidden_rel_path(&file.rel_path) {
                continue;
            }
            if !picked.contains(&file.rel_path) {
                picked.push(file.rel_path.clone());
            }
        }
    }

    picked.truncate(max);
    picked
}

fn select_hot_paths(artifact: &ProjectArtifact, delta: &DeltaSummary, max: usize) -> Vec<PathStat> {
    let mut counts: HashMap<String, usize> = HashMap::new();

    if !delta.changes.is_empty() {
        for change in &delta.changes {
            let top = top_level_path(&change.path);
            *counts.entry(top).or_insert(0) += 1;
        }
    } else {
        for file in &artifact.files {
            let top = top_level_path(&file.rel_path);
            *counts.entry(top).or_insert(0) += 1;
        }
    }

    let mut ranked: Vec<PathStat> = counts
        .into_iter()
        .map(|(path, count)| PathStat { path, count })
        .collect();
    ranked.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.path.cmp(&b.path)));
    ranked.truncate(max);
    ranked
}

fn select_top_imports(files: &[FileArtifact], max: usize) -> Vec<ImportStat> {
    let mut counts: HashMap<String, usize> = HashMap::new();

    for file in files {
        for import in &file.imports {
            if import.starts_with("self:") {
                continue;
            }
            *counts.entry(import.clone()).or_insert(0) += 1;
        }
    }

    let mut ranked: Vec<ImportStat> = counts
        .into_iter()
        .map(|(module, count)| ImportStat { module, count })
        .collect();
    ranked.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.module.cmp(&b.module)));
    ranked.truncate(max);
    ranked
}

fn top_level_path(rel_path: &str) -> String {
    rel_path
        .split('/')
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or(".")
        .to_string()
}

fn is_hidden_rel_path(rel_path: &str) -> bool {
    rel_path.split('/').any(|segment| segment.starts_with('.'))
}

/// Build the L3 api_surface slice: public symbols for given paths.
/// Falls back to top-N files by symbol count when preferred paths yield nothing.
fn build_api_surface(
    artifact: &ProjectArtifact,
    paths: &[&str],
    max_files: usize,
    max_symbols: usize,
) -> Vec<FileSurface> {
    let to_surface = |fa: &FileArtifact| -> FileSurface {
        FileSurface {
            path: fa.rel_path.clone(),
            lang: fa.language.clone().unwrap_or_else(|| "?".to_string()),
            symbols: fa.pub_symbols.iter().take(max_symbols).cloned().collect(),
        }
    };

    // Try the requested paths first
    let file_map: HashMap<&str, &FileArtifact> = artifact
        .files
        .iter()
        .map(|f| (f.rel_path.as_str(), f))
        .collect();

    let from_paths: Vec<FileSurface> = paths
        .iter()
        .filter_map(|p| file_map.get(p).copied())
        .filter(|fa| !fa.pub_symbols.is_empty())
        .take(max_files)
        .map(to_surface)
        .collect();

    if !from_paths.is_empty() {
        return from_paths;
    }

    // Fallback: top-N files with most public symbols (best API overview)
    let mut ranked: Vec<&FileArtifact> = artifact
        .files
        .iter()
        .filter(|fa| !fa.pub_symbols.is_empty())
        .collect();
    ranked.sort_by(|a, b| b.pub_symbols.len().cmp(&a.pub_symbols.len()));
    ranked.into_iter().take(max_files).map(to_surface).collect()
}

/// L1: Build module_index — compact per-module export list derived from pub_symbols.
fn build_module_index(
    artifact: &ProjectArtifact,
    max_modules: usize,
    max_exports: usize,
) -> Vec<ModuleIndexEntry> {
    let mut entries: Vec<ModuleIndexEntry> = artifact
        .files
        .iter()
        .filter(|f| !f.pub_symbols.is_empty())
        .map(|f| ModuleIndexEntry {
            module: f.rel_path.clone(),
            lang: f.language.clone().unwrap_or_else(|| "?".to_string()),
            exports: f
                .pub_symbols
                .iter()
                .take(max_exports)
                .map(|s| s.name.clone())
                .collect(),
        })
        .collect();
    entries.sort_by(|a, b| a.module.cmp(&b.module));
    entries.truncate(max_modules);
    entries
}

/// E2.3: Determine which artifact layers to include based on query_type.
fn layers_for(qt: QueryType) -> LayerFlags {
    match qt {
        QueryType::General => LayerFlags {
            l0_project_map: true,
            l1_module_index: true,
            l3_api_surface: true,
            l4_dep_manifest: true,
            l6_change_digest: true,
            top_imports: true,
        },
        QueryType::Bugfix => LayerFlags {
            l0_project_map: false,
            l1_module_index: true,
            l3_api_surface: true,
            l4_dep_manifest: false,
            l6_change_digest: true,
            top_imports: false,
        },
        QueryType::Feature => LayerFlags {
            l0_project_map: true,
            l1_module_index: true,
            l3_api_surface: true,
            l4_dep_manifest: true,
            l6_change_digest: false,
            top_imports: true,
        },
        QueryType::Refactor => LayerFlags {
            l0_project_map: false,
            l1_module_index: true,
            l3_api_surface: true,
            l4_dep_manifest: false,
            l6_change_digest: false,
            top_imports: false,
        },
        QueryType::Incident => LayerFlags {
            l0_project_map: false,
            l1_module_index: false,
            l3_api_surface: true,
            l4_dep_manifest: true,
            l6_change_digest: true,
            top_imports: false,
        },
    }
}

/// L4: Parse dependency manifest from project root. Tries Cargo.toml → package.json → pyproject.toml.
fn parse_dep_manifest(project_root: &Path) -> Option<DepManifest> {
    let cargo = project_root.join("Cargo.toml");
    if cargo.exists() {
        if let Ok(content) = fs::read_to_string(&cargo) {
            if let Some(m) = parse_cargo_toml_content(&content) {
                return Some(m);
            }
        }
    }
    let pkg = project_root.join("package.json");
    if pkg.exists() {
        if let Ok(content) = fs::read_to_string(&pkg) {
            if let Some(m) = parse_package_json_content(&content) {
                return Some(m);
            }
        }
    }
    let pyproject = project_root.join("pyproject.toml");
    if pyproject.exists() {
        if let Ok(content) = fs::read_to_string(&pyproject) {
            if let Some(m) = parse_pyproject_toml_content(&content) {
                return Some(m);
            }
        }
    }
    None
}

/// L4: Parse Cargo.toml content into DepManifest.
fn parse_cargo_toml_content(content: &str) -> Option<DepManifest> {
    let table: toml::Value = toml::from_str(content).ok()?;
    let extract = |key: &str| -> Vec<DepEntry> {
        table
            .get(key)
            .and_then(|d| d.as_table())
            .map(|t| {
                t.iter()
                    .map(|(name, val)| {
                        let version = match val {
                            toml::Value::String(v) => v.clone(),
                            toml::Value::Table(t) => t
                                .get("version")
                                .and_then(|v| v.as_str())
                                .unwrap_or("*")
                                .to_string(),
                            _ => "*".to_string(),
                        };
                        DepEntry {
                            name: name.clone(),
                            version,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    Some(DepManifest {
        runtime: extract("dependencies"),
        dev: extract("dev-dependencies"),
        build: extract("build-dependencies"),
    })
}

/// L4: Parse package.json content into DepManifest.
fn parse_package_json_content(content: &str) -> Option<DepManifest> {
    let json: serde_json::Value = serde_json::from_str(content).ok()?;
    let extract = |key: &str| -> Vec<DepEntry> {
        json.get(key)
            .and_then(|d| d.as_object())
            .map(|m| {
                m.iter()
                    .map(|(name, ver)| DepEntry {
                        name: name.clone(),
                        version: ver.as_str().unwrap_or("*").to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    Some(DepManifest {
        runtime: extract("dependencies"),
        dev: extract("devDependencies"),
        build: vec![],
    })
}

/// L4: Parse pyproject.toml content into DepManifest.
fn parse_pyproject_toml_content(content: &str) -> Option<DepManifest> {
    let table: toml::Value = toml::from_str(content).ok()?;
    let runtime: Vec<DepEntry> = table
        .get("project")
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| {
                    let (name, version) = split_pep508(s);
                    DepEntry {
                        name: name.to_string(),
                        version: version.to_string(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    Some(DepManifest {
        runtime,
        dev: vec![],
        build: vec![],
    })
}

/// Split a PEP 508 dependency specifier (e.g. "requests>=2.28") into (name, constraint).
fn split_pep508(spec: &str) -> (&str, &str) {
    let operators = [">=", "<=", "==", "!=", "~=", ">", "<", "["];
    let pos = operators.iter().filter_map(|op| spec.find(op)).min();
    match pos {
        Some(i) => (spec[..i].trim(), spec[i..].trim()),
        None => (spec.trim(), "*"),
    }
}

fn limits_for_detail(detail: DetailLevel) -> DetailLimits {
    match detail {
        DetailLevel::Compact => DetailLimits {
            max_changes: 8,
            max_entry_points: 5,
            max_hot_paths: 5,
            max_imports: 5,
            max_api_files: 5,      // L3: compact — top 5 files
            max_api_symbols: 8,    // L3: compact — 8 pub symbols each
            max_modules: 10,       // L1: compact — top 10 modules
            max_module_exports: 8, // L1: compact — 8 exports each
        },
        DetailLevel::Normal => DetailLimits {
            max_changes: 32,
            max_entry_points: 10,
            max_hot_paths: 10,
            max_imports: 12,
            max_api_files: 10,
            max_api_symbols: 20,
            max_modules: 24,
            max_module_exports: 16,
        },
        DetailLevel::Verbose => DetailLimits {
            max_changes: 256,
            max_entry_points: 32,
            max_hot_paths: 32,
            max_imports: 32,
            max_api_files: 32,
            max_api_symbols: 64,
            max_modules: 128,
            max_module_exports: 64,
        },
    }
}

fn print_response(response: &MemoryResponse, format: &str) -> Result<()> {
    match format {
        "text" => {
            println!("{}", render_text(response));
            Ok(())
        }
        "json" => {
            println!(
                "{}",
                serde_json::to_string_pretty(response)
                    .context("Failed to serialize memory response as JSON")?
            );
            Ok(())
        }
        _ => bail!("Unsupported format '{}'. Use 'text' or 'json'.", format),
    }
}

fn render_text(response: &MemoryResponse) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "memory.{} project={} id={} cache={}\n",
        response.command,
        response.project_root,
        response.project_id,
        cache_status_label(response.cache_status)
    ));
    out.push_str(&format!(
        "stats files={} bytes={} reused={} rehashed={} scanned={}\n",
        response.stats.file_count,
        format_bytes(response.stats.total_bytes),
        response.stats.reused_entries,
        response.stats.rehashed_entries,
        response.stats.scanned_files
    ));
    out.push_str(&format!(
        "delta +{} ~{} -{}\n",
        response.delta.added, response.delta.modified, response.delta.removed
    ));

    if !response.delta.files.is_empty() {
        out.push_str("changes\n");
        for change in &response.delta.files {
            out.push_str(&format!(
                "{} {}\n",
                delta_marker(change.change),
                change.path
            ));
        }
    }

    if !response.context.entry_points.is_empty() {
        out.push_str("entry_points ");
        out.push_str(&response.context.entry_points.join(", "));
        out.push('\n');
    }

    if !response.context.hot_paths.is_empty() {
        let parts: Vec<String> = response
            .context
            .hot_paths
            .iter()
            .map(|item| format!("{}({})", item.path, item.count))
            .collect();
        out.push_str("hot_paths ");
        out.push_str(&parts.join(", "));
        out.push('\n');
    }

    if !response.context.top_imports.is_empty() {
        let parts: Vec<String> = response
            .context
            .top_imports
            .iter()
            .map(|item| format!("{}({})", item.module, item.count))
            .collect();
        out.push_str("top_imports ");
        out.push_str(&parts.join(", "));
        out.push('\n');
    }

    if !response.context.api_surface.is_empty() {
        out.push_str("api_surface\n");
        for surface in &response.context.api_surface {
            let sym_str: Vec<String> = surface
                .symbols
                .iter()
                .map(|s| {
                    if let Some(sig) = &s.sig {
                        format!("{}{}({})", s.name, sig, s.kind)
                    } else {
                        format!("{}", s.name)
                    }
                })
                .collect();
            out.push_str(&format!(
                "  {}[{}]: {}\n",
                surface.path,
                surface.lang,
                sym_str.join(", ")
            ));
        }
    }

    // L1: module_index
    if !response.context.module_index.is_empty() {
        out.push_str("module_index\n");
        for entry in &response.context.module_index {
            out.push_str(&format!(
                "  {}[{}]: {}\n",
                entry.module,
                entry.lang,
                entry.exports.join(", ")
            ));
        }
    }

    // L4: dep_manifest
    if let Some(deps) = &response.context.dep_manifest {
        if !deps.runtime.is_empty() {
            let names: Vec<&str> = deps.runtime.iter().map(|d| d.name.as_str()).collect();
            out.push_str(&format!("deps_runtime {}\n", names.join(", ")));
        }
        if !deps.dev.is_empty() {
            let names: Vec<&str> = deps.dev.iter().map(|d| d.name.as_str()).collect();
            out.push_str(&format!("deps_dev {}\n", names.join(", ")));
        }
        if !deps.build.is_empty() {
            let names: Vec<&str> = deps.build.iter().map(|d| d.name.as_str()).collect();
            out.push_str(&format!("deps_build {}\n", names.join(", ")));
        }
    }

    out.push_str(&format!(
        "graph nodes={} edges={}\n",
        response.graph.nodes, response.graph.edges
    ));

    out
}

fn delta_marker(kind: DeltaKind) -> &'static str {
    match kind {
        DeltaKind::Added => "+",
        DeltaKind::Modified => "~",
        DeltaKind::Removed => "-",
    }
}

fn cache_status_label(status: CacheStatus) -> &'static str {
    match status {
        CacheStatus::Hit => "hit",
        CacheStatus::Miss => "miss",
        CacheStatus::Refreshed => "refreshed",
        CacheStatus::StaleRebuild => "stale_rebuild",
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1}GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1}MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{}B", bytes)
    }
}

fn canonical_project_root(project: &Path) -> Result<PathBuf> {
    let canonical = fs::canonicalize(project)
        .with_context(|| format!("Failed to resolve project path {}", project.display()))?;

    if !canonical.is_dir() {
        bail!("Project path must be a directory: {}", canonical.display());
    }

    Ok(canonical)
}

fn project_cache_key(project_root: &Path) -> String {
    format!(
        "{:016x}",
        xxh3_64(project_root.to_string_lossy().as_bytes())
    )
}

fn is_artifact_stale(artifact: &ProjectArtifact) -> bool {
    let now = epoch_secs(SystemTime::now());
    now.saturating_sub(artifact.updated_at) > CACHE_TTL_SECS
}

fn memory_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("rtk")
        .join("memory")
}

fn artifact_path(project_root: &Path) -> PathBuf {
    memory_cache_dir().join(format!("{}.json", project_cache_key(project_root)))
}

fn load_artifact(project_root: &Path) -> Result<Option<ProjectArtifact>> {
    let path = artifact_path(project_root);
    if !path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read cache artifact {}", path.display()))?;
    let artifact: ProjectArtifact =
        serde_json::from_str(&raw).context("Failed to parse cache artifact JSON")?;

    if artifact.version != ARTIFACT_VERSION {
        return Ok(None);
    }

    Ok(Some(artifact))
}

fn store_artifact(artifact: &ProjectArtifact) -> Result<()> {
    let cache_dir = memory_cache_dir();
    fs::create_dir_all(&cache_dir)
        .with_context(|| format!("Failed to create cache directory {}", cache_dir.display()))?;

    let path = artifact_path(Path::new(&artifact.project_root));
    let tmp = cache_dir.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        epoch_nanos(SystemTime::now())
    ));

    let json = serde_json::to_string(artifact).context("Failed to serialize cache artifact")?;
    fs::write(&tmp, json)
        .with_context(|| format!("Failed to write temporary artifact {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| {
        format!(
            "Failed to atomically replace cache artifact {}",
            path.display()
        )
    })?;

    prune_cache(&cache_dir);
    Ok(())
}

fn prune_cache(cache_dir: &Path) {
    let mut entries: Vec<(PathBuf, u64)> = match fs::read_dir(cache_dir) {
        Ok(read_dir) => read_dir
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                    return None;
                }

                let modified = entry
                    .metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .map(epoch_secs)
                    .unwrap_or(0);

                Some((path, modified))
            })
            .collect(),
        Err(_) => return,
    };

    if entries.len() <= CACHE_MAX_PROJECTS {
        return;
    }

    entries.sort_by(|a, b| a.1.cmp(&b.1));
    let remove_count = entries.len().saturating_sub(CACHE_MAX_PROJECTS);

    for (path, _) in entries.into_iter().take(remove_count) {
        let _ = fs::remove_file(path);
    }
}

fn hash_file(path: &Path) -> Result<u64> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("Failed to open file for hashing {}", path.display()))?;
    let mut hasher = Xxh3::new();
    let mut buf = [0u8; 8192];

    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("Failed to read {} while hashing", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(hasher.digest())
}

fn format_hash(value: u64) -> String {
    format!("{:016x}", value)
}

fn epoch_secs(ts: SystemTime) -> u64 {
    ts.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn epoch_nanos(ts: SystemTime) -> u128 {
    ts.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_imports_from_mixed_languages() {
        let source = r#"
use std::collections::HashMap;
import React from 'react';
const fs = require('fs');
from typing import List
import os
import "fmt"
"#;

        let imports = extract_imports(source);
        assert!(imports.contains(&"std::collections::HashMap".to_string()));
        assert!(imports.contains(&"react".to_string()));
        assert!(imports.contains(&"fs".to_string()));
        assert!(imports.contains(&"typing".to_string()));
        assert!(imports.contains(&"os".to_string()));
        assert!(imports.contains(&"fmt".to_string()));
    }

    #[test]
    fn top_level_path_handles_root_and_nested_paths() {
        assert_eq!(top_level_path("src/main.rs"), "src");
        assert_eq!(top_level_path("Cargo.toml"), "Cargo.toml");
        assert_eq!(top_level_path(""), ".");
    }

    #[test]
    fn summarize_delta_counts_each_change_kind() {
        let delta = summarize_delta(vec![
            FileDelta {
                path: "a.rs".to_string(),
                change: DeltaKind::Added,
                old_hash: None,
                new_hash: Some("1".to_string()),
            },
            FileDelta {
                path: "b.rs".to_string(),
                change: DeltaKind::Modified,
                old_hash: Some("1".to_string()),
                new_hash: Some("2".to_string()),
            },
            FileDelta {
                path: "c.rs".to_string(),
                change: DeltaKind::Removed,
                old_hash: Some("2".to_string()),
                new_hash: None,
            },
        ]);

        assert_eq!(delta.added, 1);
        assert_eq!(delta.modified, 1);
        assert_eq!(delta.removed, 1);
        assert_eq!(delta.changes.len(), 3);
    }

    #[test]
    fn chooses_entry_points_from_hints_first() {
        let files = vec![
            FileArtifact {
                rel_path: "src/util.rs".to_string(),
                size: 10,
                mtime_ns: 0,
                hash: 1,
                language: Some("rust".to_string()),
                line_count: Some(1),
                imports: vec![],
                pub_symbols: vec![],
            },
            FileArtifact {
                rel_path: "Cargo.toml".to_string(),
                size: 20,
                mtime_ns: 0,
                hash: 2,
                language: Some("toml".to_string()),
                line_count: Some(2),
                imports: vec![],
                pub_symbols: vec![],
            },
            FileArtifact {
                rel_path: "src/main.rs".to_string(),
                size: 30,
                mtime_ns: 0,
                hash: 3,
                language: Some("rust".to_string()),
                line_count: Some(3),
                imports: vec![],
                pub_symbols: vec![],
            },
        ];

        let points = select_entry_points(&files, 2);
        assert_eq!(
            points,
            vec!["Cargo.toml".to_string(), "src/main.rs".to_string()]
        );
    }

    #[test]
    fn format_bytes_thresholds() {
        assert_eq!(format_bytes(0), "0B");
        assert_eq!(format_bytes(512), "512B");
        assert_eq!(format_bytes(1024), "1.0KB");
        assert_eq!(format_bytes(1_048_576), "1.0MB");
        assert_eq!(format_bytes(1_073_741_824), "1.0GB");
    }

    #[test]
    fn detect_language_by_extension() {
        assert_eq!(
            detect_language(Path::new("foo.rs")),
            Some("rust".to_string())
        );
        assert_eq!(
            detect_language(Path::new("foo.ts")),
            Some("typescript".to_string())
        );
        assert_eq!(
            detect_language(Path::new("foo.tsx")),
            Some("typescript".to_string())
        );
        assert_eq!(
            detect_language(Path::new("foo.py")),
            Some("python".to_string())
        );
        assert_eq!(detect_language(Path::new("foo.go")), Some("go".to_string()));
        assert_eq!(detect_language(Path::new("foo.xyz")), None);
    }

    #[test]
    fn should_skip_excluded_dirs() {
        assert!(should_skip_rel_path(Path::new("node_modules/foo.js")));
        assert!(should_skip_rel_path(Path::new("target/debug/rtk")));
        assert!(should_skip_rel_path(Path::new(".git/config")));
        assert!(!should_skip_rel_path(Path::new("src/main.rs")));
        assert!(!should_skip_rel_path(Path::new("README.md")));
    }

    #[test]
    fn should_skip_rtk_lock_files() {
        assert!(should_skip_rel_path(Path::new("src/main.rs.rtk-lock")));
        assert!(!should_skip_rel_path(Path::new("src/main.rs")));
    }

    #[test]
    fn project_cache_key_is_deterministic() {
        use std::path::PathBuf;
        let path = PathBuf::from("/home/user/project");
        let key1 = project_cache_key(&path);
        let key2 = project_cache_key(&path);
        assert_eq!(key1, key2);
        assert_eq!(key1.len(), 16);
    }

    #[test]
    fn project_cache_key_differs_by_path() {
        use std::path::PathBuf;
        let key1 = project_cache_key(&PathBuf::from("/home/user/project-a"));
        let key2 = project_cache_key(&PathBuf::from("/home/user/project-b"));
        assert_ne!(key1, key2);
    }

    #[test]
    fn artifact_stale_when_older_than_ttl() {
        let old_ts = epoch_secs(SystemTime::now()).saturating_sub(CACHE_TTL_SECS + 1);
        let artifact = ProjectArtifact {
            version: ARTIFACT_VERSION,
            project_id: "test".to_string(),
            project_root: "/tmp/test".to_string(),
            created_at: old_ts,
            updated_at: old_ts,
            file_count: 0,
            total_bytes: 0,
            files: vec![],
            dep_manifest: None, // test fixture
        };
        assert!(is_artifact_stale(&artifact));
    }

    #[test]
    fn artifact_fresh_when_recently_updated() {
        let now = epoch_secs(SystemTime::now());
        let artifact = ProjectArtifact {
            version: ARTIFACT_VERSION,
            project_id: "test".to_string(),
            project_root: "/tmp/test".to_string(),
            created_at: now,
            updated_at: now,
            file_count: 0,
            total_bytes: 0,
            files: vec![],
            dep_manifest: None, // test fixture
        };
        assert!(!is_artifact_stale(&artifact));
    }

    #[test]
    fn artifact_roundtrip_json() {
        let artifact = ProjectArtifact {
            version: ARTIFACT_VERSION,
            project_id: "deadbeef00000000".to_string(),
            project_root: "/tmp/project".to_string(),
            created_at: 1_700_000_000,
            updated_at: 1_700_000_000,
            file_count: 2,
            total_bytes: 1024,
            files: vec![FileArtifact {
                rel_path: "src/main.rs".to_string(),
                size: 512,
                mtime_ns: 1_000_000,
                hash: 0xdeadbeef,
                language: Some("rust".to_string()),
                line_count: Some(20),
                imports: vec!["std::fs".to_string()],
                pub_symbols: vec![],
            }],
            dep_manifest: None, // test fixture
        };
        let json = serde_json::to_string(&artifact).expect("serialize");
        let loaded: ProjectArtifact = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(loaded.project_id, artifact.project_id);
        assert_eq!(loaded.files.len(), 1);
        assert_eq!(loaded.files[0].rel_path, "src/main.rs");
        assert_eq!(loaded.files[0].imports, vec!["std::fs"]);
    }

    #[test]
    fn parse_cargo_toml_content_extracts_deps() {
        let toml = r#"
[dependencies]
serde = { version = "1.0", features = ["derive"] }
anyhow = "1.0"

[dev-dependencies]
tempfile = "3"

[build-dependencies]
cc = "1"
"#;
        let manifest = parse_cargo_toml_content(toml).expect("valid Cargo.toml");
        assert!(manifest.runtime.iter().any(|d| d.name == "serde"));
        assert!(manifest.runtime.iter().any(|d| d.name == "anyhow"));
        assert!(manifest.dev.iter().any(|d| d.name == "tempfile"));
        assert!(manifest.build.iter().any(|d| d.name == "cc"));
        assert_eq!(
            manifest
                .runtime
                .iter()
                .find(|d| d.name == "anyhow")
                .map(|d| d.version.as_str()),
            Some("1.0")
        );
    }

    #[test]
    fn parse_cargo_toml_content_empty_sections() {
        let toml = r#"
[package]
name = "test"
version = "0.1.0"
"#;
        let manifest = parse_cargo_toml_content(toml).expect("valid Cargo.toml");
        assert!(manifest.runtime.is_empty());
        assert!(manifest.dev.is_empty());
        assert!(manifest.build.is_empty());
    }

    #[test]
    fn parse_package_json_content_extracts_deps() {
        let json = r#"{
  "dependencies": {
    "react": "^18.0.0",
    "express": "4.18.0"
  },
  "devDependencies": {
    "typescript": "5.0.0"
  }
}"#;
        let manifest = parse_package_json_content(json).expect("valid package.json");
        assert!(manifest.runtime.iter().any(|d| d.name == "react"));
        assert!(manifest.runtime.iter().any(|d| d.name == "express"));
        assert!(manifest.dev.iter().any(|d| d.name == "typescript"));
        assert!(manifest.build.is_empty());
    }

    #[test]
    fn parse_package_json_content_missing_dev_deps() {
        let json = r#"{"dependencies": {"lodash": "4.17.21"}}"#;
        let manifest = parse_package_json_content(json).expect("valid package.json");
        assert!(manifest.runtime.iter().any(|d| d.name == "lodash"));
        assert!(manifest.dev.is_empty());
    }

    #[test]
    fn parse_pyproject_toml_content_extracts_deps() {
        let toml = r#"
[project]
name = "myapp"
dependencies = ["requests>=2.28", "flask==2.0.0", "numpy"]
"#;
        let manifest = parse_pyproject_toml_content(toml).expect("valid pyproject.toml");
        assert!(manifest.runtime.iter().any(|d| d.name == "requests"));
        assert!(manifest.runtime.iter().any(|d| d.name == "flask"));
        assert!(manifest.runtime.iter().any(|d| d.name == "numpy"));
        let req = manifest
            .runtime
            .iter()
            .find(|d| d.name == "requests")
            .unwrap();
        assert_eq!(req.version, ">=2.28");
        let np = manifest.runtime.iter().find(|d| d.name == "numpy").unwrap();
        assert_eq!(np.version, "*");
    }

    #[test]
    fn split_pep508_handles_various_operators() {
        assert_eq!(split_pep508("requests>=2.28"), ("requests", ">=2.28"));
        assert_eq!(split_pep508("flask==2.0.0"), ("flask", "==2.0.0"));
        assert_eq!(split_pep508("numpy"), ("numpy", "*"));
        assert_eq!(split_pep508("pandas[excel]"), ("pandas", "[excel]"));
        assert_eq!(split_pep508("  scipy  "), ("scipy", "*"));
    }

    #[test]
    fn build_module_index_groups_by_file() {
        let artifact = ProjectArtifact {
            version: ARTIFACT_VERSION,
            project_id: "test".to_string(),
            project_root: "/tmp".to_string(),
            created_at: 0,
            updated_at: 0,
            file_count: 2,
            total_bytes: 0,
            files: vec![
                FileArtifact {
                    rel_path: "src/auth.rs".to_string(),
                    size: 100,
                    mtime_ns: 0,
                    hash: 1,
                    language: Some("rust".to_string()),
                    line_count: Some(10),
                    imports: vec![],
                    pub_symbols: vec![
                        SymbolSummary {
                            kind: "fn".to_string(),
                            name: "login".to_string(),
                            sig: None,
                        },
                        SymbolSummary {
                            kind: "struct".to_string(),
                            name: "User".to_string(),
                            sig: None,
                        },
                    ],
                },
                FileArtifact {
                    rel_path: "src/empty.rs".to_string(),
                    size: 10,
                    mtime_ns: 0,
                    hash: 2,
                    language: Some("rust".to_string()),
                    line_count: Some(1),
                    imports: vec![],
                    pub_symbols: vec![], // no exports → excluded from module_index
                },
            ],
            dep_manifest: None,
        };
        let index = build_module_index(&artifact, 10, 32);
        assert_eq!(index.len(), 1, "only files with pub_symbols should appear");
        assert_eq!(index[0].module, "src/auth.rs");
        assert!(index[0].exports.contains(&"login".to_string()));
        assert!(index[0].exports.contains(&"User".to_string()));
    }

    #[test]
    fn layers_for_bugfix_hides_dep_manifest_and_project_map() {
        let flags = layers_for(QueryType::Bugfix);
        assert!(flags.l1_module_index);
        assert!(flags.l3_api_surface);
        assert!(!flags.l4_dep_manifest);
        assert!(!flags.l0_project_map);
        assert!(!flags.top_imports);
    }

    #[test]
    fn layers_for_feature_includes_dep_manifest() {
        let flags = layers_for(QueryType::Feature);
        assert!(flags.l0_project_map);
        assert!(flags.l1_module_index);
        assert!(flags.l3_api_surface);
        assert!(flags.l4_dep_manifest);
        assert!(!flags.l6_change_digest);
    }

    #[test]
    fn layers_for_incident_shows_api_deps_and_delta() {
        let flags = layers_for(QueryType::Incident);
        assert!(!flags.l1_module_index);
        assert!(flags.l3_api_surface);
        assert!(flags.l4_dep_manifest);
        assert!(flags.l6_change_digest);
    }

    #[test]
    fn layers_for_refactor_omits_deps_and_delta() {
        let flags = layers_for(QueryType::Refactor);
        assert!(flags.l1_module_index);
        assert!(flags.l3_api_surface);
        assert!(!flags.l4_dep_manifest);
        assert!(!flags.l6_change_digest);
        assert!(!flags.l0_project_map);
    }

    // E3.1: should_watch_abs_path tests
    #[test]
    fn watch_path_ignores_excluded_dirs() {
        let project = Path::new("/home/user/myproject");
        assert!(!should_watch_abs_path(project, Path::new("/home/user/myproject/target/debug/rtk")));
        assert!(!should_watch_abs_path(project, Path::new("/home/user/myproject/node_modules/lodash/index.js")));
        assert!(!should_watch_abs_path(project, Path::new("/home/user/myproject/.git/COMMIT_EDITMSG")));
        assert!(should_watch_abs_path(project, Path::new("/home/user/myproject/src/main.rs")));
        assert!(should_watch_abs_path(project, Path::new("/home/user/myproject/Cargo.toml")));
    }

    #[test]
    fn watch_path_ignores_outside_project() {
        let project = Path::new("/home/user/myproject");
        assert!(!should_watch_abs_path(project, Path::new("/tmp/other_file.rs")));
    }

    #[test]
    fn watch_path_ignores_rtk_lock_files() {
        let project = Path::new("/home/user/myproject");
        assert!(!should_watch_abs_path(
            project,
            Path::new("/home/user/myproject/src/main.rs.rtk-lock")
        ));
    }

    #[test]
    fn layers_for_general_includes_all() {
        let flags = layers_for(QueryType::General);
        assert!(flags.l0_project_map);
        assert!(flags.l1_module_index);
        assert!(flags.l3_api_surface);
        assert!(flags.l4_dep_manifest);
        assert!(flags.top_imports);
    }
}

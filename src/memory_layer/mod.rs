use anyhow::{Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

mod api; // E4.1: HTTP /v1/* API server + daemon lifecycle
mod cache; // E0.1: persistence (load/store/prune/hash)
mod extractor;
mod indexer; // E0.1: scanning, incremental hashing, delta
mod manifest; // E0.1: dep manifest parsing
mod renderer; // E0.1: response building, text rendering, layer selection

mod budget; // E7.4: budget-aware greedy knapsack assembler
mod call_graph; // symbol call graph: who calls what (regex-based static analysis)
mod episode; // E8.1: episodic event log (debugging only, no ranking influence)
mod git_churn; // deterministic git churn frequency index (replaces affinity)
mod intent; // E7.1: task intent classifier + fingerprint
mod ollama; // E9.2: optional Ollama ML adapter (Stage-2 rerank, --ml-mode full only)
mod ranker; // E7.3: deterministic Stage-1 linear ranker

use cache::{
    canonical_project_root, delete_artifact, epoch_secs, is_artifact_stale, load_artifact,
    mem_db_path, query_cache_stats, record_cache_event, record_event, store_artifact,
    store_artifact_edges,
}; // E0.1: re-import from cache.rs (SQLite WAL, ARTIFACT_VERSION 4)
use indexer::{
    artifact_is_dirty, build_git_delta, build_state, should_watch_abs_path, summarize_graph,
}; // E0.1: re-import from indexer.rs
use renderer::{
    apply_feature_flags, build_context_slice, build_response, format_bytes, layers_for,
    limits_for_detail, print_response, render_text,
}; // E0.1: re-import from renderer.rs; E6.4: +apply_feature_flags, +layers_for

const ARTIFACT_VERSION: u32 = 4; // bumped: SQLite WAL backend (old JSON caches not migrated)
const CACHE_MAX_PROJECTS: usize = 64;
const CACHE_TTL_SECS: u64 = 24 * 60 * 60;
const IMPORT_SCAN_MAX_BYTES: u64 = 512 * 1024;
const MAX_SYMBOLS_PER_FILE: usize = 64; // L3: compile-time fallback (runtime: mem_config())
const MEM_HOOK_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/hooks/rtk-mem-context.sh"
));

/// Return the runtime memory-layer config, falling back to defaults when no config file exists.
fn mem_config() -> crate::config::MemConfig {
    crate::config::Config::load().unwrap_or_default().mem
}

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
pub(crate) struct SymbolSummary {
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

/// L5: A single test-file entry for the test_map layer.
#[derive(Debug, Serialize)]
pub(crate) struct TestMapEntry {
    path: String,
    kind: String, // "unit" | "integration" | "e2e" | "unknown"
}

/// L2: A single type relationship edge for the type_graph layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TypeRelation {
    pub(crate) source: String,   // type name (e.g. "MyStruct")
    pub(crate) target: String,   // related type (e.g. "SomeTrait")
    pub(crate) relation: String, // "implements" | "extends" | "contains" | "alias"
    pub(crate) file: String,     // source file path
}

/// L4: A single dependency entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DepEntry {
    name: String,
    version: String,
}

/// L4: Dependency manifest parsed from Cargo.toml / package.json / pyproject.toml.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct DepManifest {
    runtime: Vec<DepEntry>,
    dev: Vec<DepEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    build: Vec<DepEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum, Default)]
#[serde(rename_all = "lowercase")]
pub enum DetailLevel {
    /// Compact output (default for API and CLI) // E4.1: Default needed for ApiRequest deserialization
    #[default]
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
    l2_type_graph: bool,    // L2: type relationships (implements/extends/contains)
    l3_api_surface: bool,   // public API with signatures
    l4_dep_manifest: bool,  // dependency manifest
    l5_test_map: bool,      // test file map (L5)
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
    DirtyRebuild, // previous exists, not stale, but files changed since last index
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactFreshness {
    Fresh,
    Stale,
    Dirty,
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
    /// P0 dirty-blocking: explicit freshness state in response (PRD §8)
    freshness: &'static str,
    stats: ProjectStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    delta: Option<DeltaPayload>,
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    type_graph: Vec<TypeRelation>, // L2: type relationships
    #[serde(skip_serializing_if = "Option::is_none")]
    dep_manifest: Option<DepManifest>, // L4: dependency manifest
    #[serde(skip_serializing_if = "Vec::is_empty")]
    test_map: Vec<TestMapEntry>, // L5: test file map
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
pub(crate) struct FileArtifact {
    rel_path: String,
    size: u64,
    mtime_ns: u64,
    hash: u64,
    language: Option<String>,
    line_count: Option<u32>,
    imports: Vec<String>,
    #[serde(default)]
    pub_symbols: Vec<SymbolSummary>, // L3: cached public API surface
    #[serde(default)]
    type_relations: Vec<TypeRelation>, // L2: cached type graph edges
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

/// E6.3: Token savings statistics for `rtk memory gain`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct GainStats {
    pub raw_bytes: u64,
    pub context_bytes: u64,
    pub savings_pct: f64,
    pub files_indexed: usize,
}

/// E6.3: Compute token savings by comparing raw file bytes to rendered context size.
/// Pure function — no I/O, fully testable.
fn compute_gain_stats(artifact: &ProjectArtifact, detail: DetailLevel) -> GainStats {
    let raw_bytes = artifact.total_bytes;

    if artifact.files.is_empty() {
        return GainStats {
            raw_bytes: 0,
            context_bytes: 0,
            savings_pct: 0.0,
            files_indexed: 0,
        };
    }

    // Build a synthetic response to measure rendered context size
    let empty_delta = DeltaSummary {
        added: 0,
        modified: 0,
        removed: 0,
        changes: vec![],
    };
    let limits = limits_for_detail(detail);
    // E6.4: gain measures full-featured context (all flags on) to show maximum savings potential
    let gain_layers = apply_feature_flags(
        layers_for(QueryType::General),
        &crate::config::MemFeatureFlags::default(),
    );
    let context = build_context_slice(artifact, &empty_delta, limits, gain_layers); // E6.4: pre-computed layers

    // Render to text and measure byte length
    let graph = summarize_graph(artifact);
    let response = MemoryResponse {
        command: "gain".to_string(),
        project_root: artifact.project_root.clone(),
        project_id: artifact.project_id.clone(),
        artifact_version: artifact.version,
        detail,
        cache_status: CacheStatus::Hit,
        cache_hit: true,
        freshness: "fresh", // P0: synthetic response for gain measurement
        stats: ProjectStats {
            file_count: artifact.file_count,
            total_bytes: artifact.total_bytes,
            reused_entries: artifact.file_count,
            rehashed_entries: 0,
            scanned_files: artifact.file_count,
        },
        delta: Some(DeltaPayload {
            added: 0,
            modified: 0,
            removed: 0,
            files: vec![],
        }),
        context,
        graph,
    };
    let rendered = render_text(&response);
    let context_bytes = rendered.len() as u64;

    let savings_pct = if raw_bytes > 0 {
        (1.0 - (context_bytes as f64 / raw_bytes as f64)) * 100.0
    } else {
        0.0
    };

    GainStats {
        raw_bytes,
        context_bytes,
        savings_pct: savings_pct.max(0.0), // clamp negative (context > raw) to 0
        files_indexed: artifact.files.len(),
    }
}

pub fn run_explore(
    project: &Path,
    refresh: bool,
    strict: bool, // P1: strict dirty-blocking per PRD §8
    detail: DetailLevel,
    format: &str,
    query_type: QueryType, // E2.3: relevance-layer filtering
    verbose: u8,
) -> Result<()> {
    let cfg = mem_config(); // E6.4: read feature flags once per invocation
    let effective_strict = strict || cfg.features.strict_by_default; // E6.4: strict_by_default
    let state = build_state(project, refresh, cfg.features.cascade_invalidation, verbose)?; // E6.4: cascade flag

    // P1: Strict dirty-blocking (PRD §8) — refuse to serve stale/dirty data
    if effective_strict && !refresh {
        // E6.4: use effective_strict
        if state.stale_previous {
            anyhow::bail!(
                "memory.explore --strict: artifact is STALE (TTL expired). \
                 Run `rtk memory refresh` or omit --strict to auto-rebuild."
            );
        }
        if state.previous_exists && !state.delta.changes.is_empty() {
            anyhow::bail!(
                "memory.explore --strict: artifact is DIRTY ({} files changed since last index). \
                 Run `rtk memory refresh` or omit --strict to auto-rebuild.",
                state.delta.changes.len()
            );
        }
    }

    let should_store = refresh || !state.cache_hit;
    if should_store {
        store_artifact(&state.artifact)?;
        // E3.2: store import edges for cascade invalidation
        store_import_edges(&state.artifact);
    }

    // Warn on stderr when serving rebuilt data (PRD §8)
    if state.stale_previous && verbose > 0 {
        eprintln!("memory.explore WARNING: stale artifact rebuilt from current FS");
    }
    if state.previous_exists && !state.stale_previous && !state.delta.changes.is_empty() {
        eprintln!(
            "memory.explore NOTICE: {} files changed since last index, rebuilt",
            state.delta.changes.len()
        );
    }

    // E1.4: record cache event for analytics
    let event_label = cache_status_event_label(&state, refresh);
    let _ = record_cache_event(&state.project_id, event_label);
    let _ = record_event(&state.project_id, "explore", None);

    let response = build_response(
        "explore",
        &state,
        detail,
        refresh,
        &state.delta,
        query_type,
        &cfg.features,
    ); // E6.4
    print_response(&response, format)
}

pub fn run_delta(
    project: &Path,
    since: Option<&str>,
    detail: DetailLevel,
    format: &str,
    query_type: QueryType, // E2.3
    verbose: u8,
) -> Result<()> {
    let cfg = mem_config(); // E6.4: read feature flags once
    let state = build_state(project, false, cfg.features.cascade_invalidation, verbose)?; // E6.4
    if !state.delta.changes.is_empty() {
        store_artifact(&state.artifact)?;
    }

    let response_delta = if let Some(rev) = since {
        // E6.4: git_delta feature flag guard
        if !cfg.features.git_delta {
            anyhow::bail!(
                "memory.delta --since: git delta is disabled via [mem.features] git_delta = false. \
                 Enable it in ~/.config/rtk/config.toml or omit --since to use FS delta."
            );
        }
        build_git_delta(&state.project_root, rev, verbose)?
    } else {
        state.delta.clone()
    };

    // E1.4: record cache event
    let _ = record_cache_event(&state.project_id, "delta");

    let response = build_response(
        "delta",
        &state,
        detail,
        false,
        &response_delta,
        query_type,
        &cfg.features,
    ); // E6.4
    print_response(&response, format)
}

pub fn run_refresh(
    project: &Path,
    detail: DetailLevel,
    format: &str,
    query_type: QueryType, // E2.3
    verbose: u8,
) -> Result<()> {
    let cfg = mem_config(); // E6.4: read feature flags once
    let state = build_state(project, true, cfg.features.cascade_invalidation, verbose)?; // E6.4
    store_artifact(&state.artifact)?;
    store_import_edges(&state.artifact); // E3.2: refresh edges
    let _ = record_cache_event(&state.project_id, "refreshed"); // E1.4

    let response = build_response(
        "refresh",
        &state,
        detail,
        true,
        &state.delta,
        query_type,
        &cfg.features,
    ); // E6.4
    print_response(&response, format)
}

pub fn run_watch(
    project: &Path,
    interval_secs: u64, // E3.1: debounce window in seconds (was: poll interval)
    detail: DetailLevel,
    format: &str,
    query_type: QueryType, // E2.3
    verbose: u8,
) -> Result<()> {
    use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher}; // E3.1
    use std::sync::mpsc;
    use std::time::Instant;

    let cfg = mem_config(); // E6.4: read feature flags once for watch lifecycle
    let debounce = Duration::from_secs(interval_secs.max(1)); // E3.1: debounce window
    let project = project
        .canonicalize()
        .unwrap_or_else(|_| project.to_path_buf());

    // Initial snapshot before registering the watcher
    {
        let state = build_state(&project, false, cfg.features.cascade_invalidation, verbose)?; // E6.4
        if !state.delta.changes.is_empty() || state.stale_previous || !state.previous_exists {
            store_artifact(&state.artifact)?;
            let response = build_response(
                "watch",
                &state,
                detail,
                false,
                &state.delta,
                query_type,
                &cfg.features,
            ); // E6.4
            print_response(&response, format)?;
        }
    }

    // E3.1: set up event-driven watcher (kqueue on macOS, inotify on Linux)
    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
    let mut watcher = RecommendedWatcher::new(
        move |res| {
            let _ = tx.send(res);
        },
        Config::default(),
    )
    .context("Failed to create filesystem watcher")?;
    watcher
        .watch(&project, RecursiveMode::Recursive)
        .context("Failed to watch project directory")?;

    if verbose > 0 {
        eprintln!(
            "memory.watch start project={} debounce={}s backend=notify",
            project.to_string_lossy(),
            debounce.as_secs(),
        );
    }

    loop {
        // Block until first relevant FS event arrives
        let got_relevant = loop {
            match rx.recv().context("Watcher channel closed")? {
                Ok(event) => {
                    // E3.1: filter out events from excluded dirs
                    if event
                        .paths
                        .iter()
                        .any(|p| should_watch_abs_path(&project, p))
                    {
                        break true;
                    }
                    // irrelevant path (target/, node_modules/, etc.) — keep waiting
                }
                Err(e) => {
                    if verbose > 0 {
                        eprintln!("memory.watch error: {e}");
                    }
                    break false; // log error, try again
                }
            }
        };

        if !got_relevant {
            continue;
        }

        // E3.1: coalesce additional events within the debounce window
        let deadline = Instant::now() + debounce;
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(anyhow::anyhow!("Watcher channel disconnected"));
                }
            }
        }

        // Build updated state and emit if anything changed
        let state = build_state(&project, false, cfg.features.cascade_invalidation, verbose)?; // E6.4
        if !state.delta.changes.is_empty() || state.stale_previous {
            store_artifact(&state.artifact)?;
            let response = build_response(
                "watch",
                &state,
                detail,
                false,
                &state.delta,
                query_type,
                &cfg.features,
            ); // E6.4
            print_response(&response, format)?;
        } else if verbose > 0 {
            eprintln!("memory.watch project={} clean", project.to_string_lossy());
        }
    }
}

fn is_mem_hook_entry(entry: &serde_json::Value) -> bool {
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
}

fn installed_mem_hook_command(pre: &[serde_json::Value]) -> Option<String> {
    pre.iter()
        .find(|entry| is_mem_hook_entry(entry))
        .and_then(|entry| entry.get("hooks"))
        .and_then(|hooks| hooks.as_array())
        .and_then(|hooks| hooks.first())
        .and_then(|hook| hook.get("command"))
        .and_then(|command| command.as_str())
        .map(|s| s.to_string())
}

fn materialize_mem_hook_script() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Cannot find home directory")?;
    let hooks_dir = home.join(".claude").join("hooks");
    fs::create_dir_all(&hooks_dir)
        .with_context(|| format!("Failed to create hooks directory {}", hooks_dir.display()))?;

    let hook_path = hooks_dir.join("rtk-mem-context.sh");
    fs::write(&hook_path, MEM_HOOK_SCRIPT)
        .with_context(|| format!("Failed to write {}", hook_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(&hook_path, perms).with_context(|| {
            format!(
                "Failed to set executable permissions on {}",
                hook_path.display()
            )
        })?;
    }

    Ok(hook_path)
}

/// Install (or uninstall) the rtk-mem-context.sh PreToolUse:Task hook in ~/.claude/settings.json
pub fn run_install_hook(uninstall: bool, status_only: bool, verbose: u8) -> Result<()> {
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

    // Read current PreToolUse array
    let pre = settings
        .get("hooks")
        .and_then(|h| h.get("PreToolUse"))
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    let existing_hook = installed_mem_hook_command(&pre);
    let already_installed = existing_hook.is_some();

    if status_only {
        println!(
            "memory.hook status={} path={} command={}",
            if already_installed {
                "installed"
            } else {
                "not_installed"
            },
            settings_path.display(),
            existing_hook.unwrap_or_else(|| "-".to_string())
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
            .filter(|entry| !is_mem_hook_entry(entry))
            .collect();
        settings["hooks"]["PreToolUse"] = serde_json::json!(filtered);
        // P1: backup before uninstall write
        if settings_path.exists() {
            let backup = settings_path.with_extension("json.bak");
            let _ = fs::copy(&settings_path, &backup);
        }
        let json = serde_json::to_string_pretty(&settings)?;
        fs::write(&settings_path, json)?;
        println!("memory.hook uninstall ok path={}", settings_path.display());
        return Ok(());
    }

    let hook_bin = materialize_mem_hook_script()?;
    let hook_entry = serde_json::json!({
        "matcher": "Task",
        "hooks": [{
            "type": "command",
            "command": hook_bin.to_string_lossy().to_string(),
            "timeout": 10
        }]
    });

    // Upsert the hook entry (repairs stale/invalid command paths)
    let mut new_pre: Vec<serde_json::Value> = pre
        .into_iter()
        .filter(|entry| !is_mem_hook_entry(entry))
        .collect();
    new_pre.push(hook_entry);
    settings["hooks"]["PreToolUse"] = serde_json::json!(new_pre);

    // Ensure parent directory exists
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // P1: backup settings.json before modifying (prevent data loss on corruption)
    if settings_path.exists() {
        let backup = settings_path.with_extension("json.bak");
        if let Err(e) = fs::copy(&settings_path, &backup) {
            eprintln!(
                "memory.hook WARNING: failed to create backup {}: {}",
                backup.display(),
                e
            );
        }
    }

    let json = serde_json::to_string_pretty(&settings)?;
    fs::write(&settings_path, &json)
        .with_context(|| format!("Failed to write {}", settings_path.display()))?;

    println!(
        "memory.hook {} path={}",
        if already_installed {
            "updated ok"
        } else {
            "installed ok"
        },
        settings_path.display()
    );
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
            let freshness = if is_artifact_stale(&a) {
                ArtifactFreshness::Stale
            } else if artifact_is_dirty(&project_root, &a)? {
                ArtifactFreshness::Dirty
            } else {
                ArtifactFreshness::Fresh
            };
            let age_secs = epoch_secs(SystemTime::now()).saturating_sub(a.updated_at);
            println!(
                "memory.status project={} id={} cache={} files={} bytes={} updated={}s ago",
                project_root.display(),
                a.project_id,
                freshness_label(freshness),
                a.file_count,
                format_bytes(a.total_bytes),
                age_secs
            );
            if verbose > 0 {
                let db = mem_db_path(); // SQLite WAL — show db path instead of json file
                println!("  db: {}", db.display());
                println!("  version: {}", a.version);
                // E1.4: show cache_stats aggregate
                if let Ok(stats) = query_cache_stats(&a.project_id) {
                    if !stats.is_empty() {
                        let pairs: Vec<String> =
                            stats.iter().map(|(e, c)| format!("{}={}", e, c)).collect();
                        println!("  stats: {}", pairs.join(" "));
                    }
                }
            }
        }
    }
    Ok(())
}

fn freshness_label(freshness: ArtifactFreshness) -> &'static str {
    match freshness {
        ArtifactFreshness::Fresh => "fresh",
        ArtifactFreshness::Stale => "stale",
        ArtifactFreshness::Dirty => "dirty",
    }
}

/// E1.4: Derive cache event label from build state for cache_stats recording.
fn cache_status_event_label(state: &BuildState, refresh: bool) -> &'static str {
    if refresh {
        "refreshed"
    } else if state.stale_previous {
        "stale_rebuild"
    } else if state.cache_hit {
        "hit"
    } else if state.previous_exists && !state.delta.changes.is_empty() {
        "dirty_rebuild"
    } else {
        "miss"
    }
}

/// E3.2: Extract import edges from artifact and store in artifact_edges table.
fn store_import_edges(artifact: &ProjectArtifact) {
    let mut edges: Vec<(String, String)> = Vec::new();
    for file in &artifact.files {
        for import in &file.imports {
            if import.starts_with("self:") {
                continue; // skip synthetic anchors
            }
            edges.push((file.rel_path.clone(), import.clone()));
        }
    }
    let _ = store_artifact_edges(&artifact.project_id, &edges);
}

pub fn run_clear(project: &Path, _verbose: u8) -> Result<()> {
    let project_root = canonical_project_root(project)?;
    // SQLite WAL: delete rows instead of removing json file
    if delete_artifact(&project_root)? {
        println!("memory.clear project={} ok", project_root.display());
    } else {
        println!(
            "memory.clear project={} nothing to clear",
            project_root.display()
        );
    }
    Ok(())
}

/// E6.3: Show token savings — raw source bytes vs compact context bytes.
/// E4.1: Start localhost HTTP API server with idle-timeout daemon lifecycle.
pub fn run_serve(port: u16, idle_secs: u64, verbose: u8) -> Result<()> {
    api::serve(port, idle_secs, verbose)
}

/// CLI entry for `rtk memory plan` — ranked context under token budget.
pub fn run_plan(
    project: &Path,
    task: &str,
    token_budget: u32,
    format: &str,
    _verbose: u8,
) -> Result<()> {
    use std::collections::HashSet;

    let project_root = canonical_project_root(project)?;
    let cfg = mem_config();
    let token_budget = if token_budget == 0 {
        4000
    } else {
        token_budget
    };

    // Build/reuse artifact
    let state = indexer::build_state(&project_root, false, cfg.features.cascade_invalidation, 0)?;
    if !state.cache_hit {
        store_artifact(&state.artifact)?;
        store_import_edges(&state.artifact);
    }

    // Load git churn (cached by HEAD sha)
    let churn = git_churn::load_churn(&project_root).unwrap_or_else(|_| git_churn::ChurnCache {
        head_sha: "unknown".to_string(),
        freq_map: std::collections::HashMap::new(),
        max_count: 0,
    });

    // Parse intent for weight tuning
    let parsed_intent = intent::parse_intent(task, &state.project_id);

    // Recent-change paths for f_recency_score
    let recent_paths: HashSet<String> =
        state.delta.changes.iter().map(|d| d.path.clone()).collect();

    // Build call graph from artifact pub fn symbols
    let all_symbols: Vec<(String, Vec<String>)> = state
        .artifact
        .files
        .iter()
        .map(|fa| {
            let syms: Vec<String> = fa
                .pub_symbols
                .iter()
                .filter(|s| s.kind == "fn")
                .map(|s| s.name.clone())
                .collect();
            (fa.rel_path.clone(), syms)
        })
        .collect();
    let cg = call_graph::CallGraph::build(&all_symbols, &project_root);
    let query_tags = parsed_intent.extracted_tags.clone();

    // Build candidates
    let candidates: Vec<ranker::Candidate> = state
        .artifact
        .files
        .iter()
        .map(|fa| {
            let mut c = ranker::Candidate::new(&fa.rel_path);
            c.features.f_structural_relevance = if !fa.pub_symbols.is_empty() {
                1.0
            } else if !fa.imports.is_empty() {
                0.5
            } else {
                0.2
            };
            c.features.f_churn_score = git_churn::churn_score(&churn, &fa.rel_path);
            c.features.f_recency_score = if recent_paths.contains(&fa.rel_path) {
                1.0
            } else {
                0.0
            };
            c.features.f_risk_score = ranker::path_risk_score(&fa.rel_path);
            c.features.f_test_proximity = if ranker::is_test_file(&fa.rel_path) {
                0.8
            } else {
                0.0
            };
            c.features.f_call_graph_score = cg.caller_score(&fa.rel_path, &query_tags);
            let raw_cost = budget::estimate_tokens_for_path(&fa.rel_path);
            c.estimated_tokens = raw_cost;
            c.features.f_token_cost = (raw_cost as f32 / 1000.0).min(1.0);
            c.sources.push("artifact".to_string());
            c
        })
        .collect();

    // Stage-1 rank
    let ranked = ranker::rank_stage1(candidates, &parsed_intent);

    // Budget-aware assembly
    let result = budget::assemble(ranked, token_budget);

    if format == "json" {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "# Plan Context ({} selected, {}/{} tokens)",
            result.budget_report.candidates_selected,
            result.budget_report.estimated_used,
            token_budget
        );
        for c in &result.selected {
            println!("  [{:.2}] {}", c.score, c.rel_path);
        }
        if !result.dropped.is_empty() {
            println!("# Dropped: {}", result.dropped.len());
        }
    }
    Ok(())
}

pub fn run_gain(project: &Path, verbose: u8) -> Result<()> {
    let project_root = canonical_project_root(project)?;
    let artifact = load_artifact(&project_root)?;

    match artifact {
        None => {
            println!(
                "memory.gain project={} cache=miss (run `rtk memory explore` first)",
                project_root.display()
            );
        }
        Some(a) => {
            let freshness = if is_artifact_stale(&a) {
                ArtifactFreshness::Stale
            } else if artifact_is_dirty(&project_root, &a)? {
                ArtifactFreshness::Dirty
            } else {
                ArtifactFreshness::Fresh
            };
            let compact = compute_gain_stats(&a, DetailLevel::Compact);

            println!(
                "memory.gain project={} cache={} files={}",
                project_root.display(),
                freshness_label(freshness),
                compact.files_indexed,
            );
            println!(
                "  raw_source: {} ({} bytes)",
                format_bytes(compact.raw_bytes),
                compact.raw_bytes,
            );
            println!(
                "  context:    {} ({} bytes)",
                format_bytes(compact.context_bytes),
                compact.context_bytes,
            );
            println!("  savings:    {:.1}%", compact.savings_pct);

            // -v: compare all detail levels
            if verbose > 0 {
                let normal = compute_gain_stats(&a, DetailLevel::Normal);
                let full = compute_gain_stats(&a, DetailLevel::Verbose);
                println!("  --- detail level comparison ---");
                println!(
                    "  compact:  {} ({:.1}% savings)",
                    format_bytes(compact.context_bytes),
                    compact.savings_pct,
                );
                println!(
                    "  normal:   {} ({:.1}% savings)",
                    format_bytes(normal.context_bytes),
                    normal.savings_pct,
                );
                println!(
                    "  verbose:  {} ({:.1}% savings)",
                    format_bytes(full.context_bytes),
                    full.savings_pct,
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::cache::{epoch_secs, is_artifact_stale, project_cache_key}; // E0.1: from cache.rs
    use super::extractor; // E0.1: access moved functions
    use super::indexer::{should_skip_rel_path, should_watch_abs_path, summarize_delta}; // E0.1: from indexer.rs
    use super::manifest;
    use super::renderer::{
        build_module_index, build_response, layers_for, select_entry_points, top_level_path,
    }; // E0.1: from renderer.rs
    use super::*;
    use super::{cache, indexer, renderer}; // module-level access for new tests

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

        let imports = extractor::extract_imports(source);
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
                type_relations: vec![],
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
                type_relations: vec![],
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
                type_relations: vec![],
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
                type_relations: vec![],
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
    fn parse_cargo_toml_content_empty_sections() {
        let toml = r#"
[package]
name = "test"
version = "0.1.0"
"#;
        let manifest = manifest::parse_cargo_toml_content(toml).expect("valid Cargo.toml");
        assert!(manifest.runtime.is_empty());
        assert!(manifest.dev.is_empty());
        assert!(manifest.build.is_empty());
    }

    #[test]
    fn parse_package_json_content_missing_dev_deps() {
        let json = r#"{"dependencies": {"lodash": "4.17.21"}}"#;
        let manifest = manifest::parse_package_json_content(json).expect("valid package.json");
        assert!(manifest.runtime.iter().any(|d| d.name == "lodash"));
        assert!(manifest.dev.is_empty());
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
                    type_relations: vec![],
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
                    type_relations: vec![],
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
        assert!(!should_watch_abs_path(
            project,
            Path::new("/home/user/myproject/target/debug/rtk")
        ));
        assert!(!should_watch_abs_path(
            project,
            Path::new("/home/user/myproject/node_modules/lodash/index.js")
        ));
        assert!(!should_watch_abs_path(
            project,
            Path::new("/home/user/myproject/.git/COMMIT_EDITMSG")
        ));
        assert!(should_watch_abs_path(
            project,
            Path::new("/home/user/myproject/src/main.rs")
        ));
        assert!(should_watch_abs_path(
            project,
            Path::new("/home/user/myproject/Cargo.toml")
        ));
    }

    #[test]
    fn watch_path_ignores_outside_project() {
        let project = Path::new("/home/user/myproject");
        assert!(!should_watch_abs_path(
            project,
            Path::new("/tmp/other_file.rs")
        ));
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

    #[test]
    fn build_response_hides_delta_when_l6_disabled() {
        let state = BuildState {
            project_root: PathBuf::from("/tmp/proj"),
            project_id: "p1".to_string(),
            previous_exists: true,
            stale_previous: false,
            cache_hit: true,
            scan_stats: ScanStats {
                scanned_files: 1,
                reused_entries: 1,
                rehashed_entries: 0,
            },
            artifact: ProjectArtifact {
                version: ARTIFACT_VERSION,
                project_id: "p1".to_string(),
                project_root: "/tmp/proj".to_string(),
                created_at: 0,
                updated_at: 0,
                file_count: 1,
                total_bytes: 12,
                files: vec![FileArtifact {
                    rel_path: "src/main.rs".to_string(),
                    size: 12,
                    mtime_ns: 0,
                    hash: 1,
                    language: Some("rust".to_string()),
                    line_count: Some(1),
                    imports: vec![],
                    pub_symbols: vec![],
                    type_relations: vec![],
                }],
                dep_manifest: None,
            },
            delta: DeltaSummary {
                added: 0,
                modified: 1,
                removed: 0,
                changes: vec![FileDelta {
                    path: "src/main.rs".to_string(),
                    change: DeltaKind::Modified,
                    old_hash: Some("01".to_string()),
                    new_hash: Some("02".to_string()),
                }],
            },
            graph: GraphSummary { nodes: 1, edges: 0 },
        };

        let feature = build_response(
            "explore",
            &state,
            DetailLevel::Compact,
            false,
            &state.delta,
            QueryType::Feature,
            &crate::config::MemFeatureFlags::default(), // E6.4: test uses defaults
        );
        assert!(
            feature.delta.is_none(),
            "feature query must hide L6 change digest"
        );

        let bugfix = build_response(
            "explore",
            &state,
            DetailLevel::Compact,
            false,
            &state.delta,
            QueryType::Bugfix,
            &crate::config::MemFeatureFlags::default(), // E6.4: test uses defaults
        );
        assert!(bugfix.delta.is_some(), "bugfix query must include L6 delta");
    }

    #[test]
    fn render_text_omits_delta_section_when_absent() {
        let response = MemoryResponse {
            command: "explore".to_string(),
            project_root: "/tmp/p".to_string(),
            project_id: "pid".to_string(),
            artifact_version: ARTIFACT_VERSION,
            detail: DetailLevel::Compact,
            cache_status: CacheStatus::Hit,
            cache_hit: true,
            freshness: "fresh", // P0: explicit freshness in test fixture
            stats: ProjectStats {
                file_count: 1,
                total_bytes: 1,
                reused_entries: 1,
                rehashed_entries: 0,
                scanned_files: 1,
            },
            delta: None,
            context: ContextSlice {
                entry_points: vec![],
                hot_paths: vec![],
                top_imports: vec![],
                api_surface: vec![],
                module_index: vec![],
                type_graph: vec![], // L2
                dep_manifest: None,
                test_map: vec![], // L5
            },
            graph: GraphSummary { nodes: 1, edges: 0 },
        };
        let text = render_text(&response);
        assert!(!text.contains("delta +"), "delta section should be hidden");
    }

    // E6.3: compute_gain_stats tests (RED phase)
    #[test]
    fn gain_stats_empty_artifact_zero_savings() {
        let artifact = ProjectArtifact {
            version: ARTIFACT_VERSION,
            project_id: "test".to_string(),
            project_root: "/tmp/test".to_string(),
            created_at: 0,
            updated_at: 0,
            file_count: 0,
            total_bytes: 0,
            files: vec![],
            dep_manifest: None,
        };
        let stats = compute_gain_stats(&artifact, DetailLevel::Compact);
        assert_eq!(stats.raw_bytes, 0);
        assert_eq!(stats.context_bytes, 0);
        assert_eq!(stats.savings_pct, 0.0);
        assert_eq!(stats.files_indexed, 0);
    }

    #[test]
    fn gain_stats_nonzero_files_shows_savings() {
        let artifact = ProjectArtifact {
            version: ARTIFACT_VERSION,
            project_id: "test".to_string(),
            project_root: "/tmp/test".to_string(),
            created_at: 0,
            updated_at: 0,
            file_count: 2,
            total_bytes: 10_000,
            files: vec![
                FileArtifact {
                    rel_path: "src/main.rs".to_string(),
                    size: 5000,
                    mtime_ns: 0,
                    hash: 1,
                    language: Some("rust".to_string()),
                    line_count: Some(100),
                    imports: vec!["std::fs".to_string()],
                    pub_symbols: vec![SymbolSummary {
                        kind: "fn".to_string(),
                        name: "main".to_string(),
                        sig: Some("fn main()".to_string()),
                    }],
                    type_relations: vec![],
                },
                FileArtifact {
                    rel_path: "src/lib.rs".to_string(),
                    size: 5000,
                    mtime_ns: 0,
                    hash: 2,
                    language: Some("rust".to_string()),
                    line_count: Some(100),
                    imports: vec!["serde".to_string()],
                    pub_symbols: vec![SymbolSummary {
                        kind: "struct".to_string(),
                        name: "Config".to_string(),
                        sig: None,
                    }],
                    type_relations: vec![],
                },
            ],
            dep_manifest: None,
        };
        let stats = compute_gain_stats(&artifact, DetailLevel::Compact);
        assert_eq!(stats.raw_bytes, 10_000);
        assert!(stats.context_bytes > 0, "context should not be empty");
        assert!(stats.context_bytes < stats.raw_bytes, "context < raw");
        assert!(stats.savings_pct > 0.0, "should have token savings");
        assert_eq!(stats.files_indexed, 2);
    }

    // ── SQLite concurrent access ──────────────────────────────────────────────

    #[test]
    fn sqlite_concurrent_store_and_load() {
        use std::thread;

        // Use isolated fake paths so this test doesn't interfere with real projects.
        // Each thread stores its own artifact and reads it back.
        let handles: Vec<_> = (0..2)
            .map(|i| {
                thread::spawn(move || {
                    let fake_root = format!("/tmp/rtk_test_conc_{}", i);
                    let project_id = cache::project_cache_key(std::path::Path::new(&fake_root));
                    let artifact = ProjectArtifact {
                        version: ARTIFACT_VERSION,
                        project_id,
                        project_root: fake_root.clone(),
                        created_at: 0,
                        updated_at: 0,
                        file_count: 0,
                        total_bytes: 0,
                        files: vec![],
                        dep_manifest: None,
                    };
                    store_artifact(&artifact).expect("store_artifact should succeed");
                    let loaded = load_artifact(std::path::Path::new(&fake_root))
                        .expect("load_artifact should succeed");
                    assert!(loaded.is_some(), "thread {} should reload its artifact", i);
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread panicked");
        }
    }

    // ── DirtyRebuild status ───────────────────────────────────────────────────

    #[test]
    fn dirty_rebuild_cache_status_appears_on_file_change() {
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let src_file = dir.path().join("lib.rs");
        std::fs::write(&src_file, "pub fn hello() {}").unwrap();

        // Build initial state and store it so the next build sees a previous artifact.
        let state0 = indexer::build_state(dir.path(), false, true, 0)
            .expect("initial build_state should succeed");
        store_artifact(&state0.artifact).expect("initial store should succeed");

        // Ensure mtime will differ on the next write
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(&src_file, "pub fn hello() { println!(\"changed\"); }").unwrap();

        // Second build — previous exists, not stale, but has changes → DirtyRebuild
        let state1 = indexer::build_state(dir.path(), false, true, 0)
            .expect("second build_state should succeed");
        let response = renderer::build_response(
            "explore",
            &state1,
            DetailLevel::Compact,
            false,
            &state1.delta,
            QueryType::General,
            &crate::config::MemFeatureFlags::default(), // E6.4: test uses defaults
        );
        assert_eq!(
            response.cache_status,
            CacheStatus::DirtyRebuild,
            "changed file should produce DirtyRebuild status"
        );
    }

    #[test]
    fn gain_stats_verbose_larger_than_compact() {
        let artifact = ProjectArtifact {
            version: ARTIFACT_VERSION,
            project_id: "test".to_string(),
            project_root: "/tmp/test".to_string(),
            created_at: 0,
            updated_at: 0,
            file_count: 1,
            total_bytes: 3000,
            files: vec![FileArtifact {
                rel_path: "src/main.rs".to_string(),
                size: 3000,
                mtime_ns: 0,
                hash: 1,
                language: Some("rust".to_string()),
                line_count: Some(60),
                imports: vec!["std::io".to_string(), "anyhow".to_string()],
                pub_symbols: vec![
                    SymbolSummary {
                        kind: "fn".to_string(),
                        name: "run".to_string(),
                        sig: Some("fn run() -> Result<()>".to_string()),
                    },
                    SymbolSummary {
                        kind: "fn".to_string(),
                        name: "main".to_string(),
                        sig: Some("fn main()".to_string()),
                    },
                ],
                type_relations: vec![],
            }],
            dep_manifest: None,
        };
        let compact = compute_gain_stats(&artifact, DetailLevel::Compact);
        let verbose = compute_gain_stats(&artifact, DetailLevel::Verbose);
        assert!(
            verbose.context_bytes >= compact.context_bytes,
            "verbose ({}) should be >= compact ({})",
            verbose.context_bytes,
            compact.context_bytes
        );
    }

    // ── P0: freshness field tests ────────────────────────────────────────────

    #[test]
    fn response_freshness_is_fresh_on_cache_hit() {
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("main.rs");
        std::fs::write(&src, "fn main() {}").unwrap();

        let state = indexer::build_state(dir.path(), false, true, 0).unwrap();
        store_artifact(&state.artifact).unwrap();

        // Second build — no changes — cache hit
        let state2 = indexer::build_state(dir.path(), false, true, 0).unwrap();
        let response = renderer::build_response(
            "explore",
            &state2,
            DetailLevel::Compact,
            false,
            &state2.delta,
            QueryType::General,
            &crate::config::MemFeatureFlags::default(), // E6.4: test uses defaults
        );
        assert_eq!(
            response.freshness, "fresh",
            "cache hit should yield freshness=fresh"
        );
        assert_eq!(response.cache_status, CacheStatus::Hit);
    }

    #[test]
    fn response_freshness_is_rebuilt_on_dirty() {
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("lib.rs");
        std::fs::write(&src, "pub fn a() {}").unwrap();

        let state0 = indexer::build_state(dir.path(), false, true, 0).unwrap();
        store_artifact(&state0.artifact).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(&src, "pub fn b() {}").unwrap();

        let state1 = indexer::build_state(dir.path(), false, true, 0).unwrap();
        let response = renderer::build_response(
            "explore",
            &state1,
            DetailLevel::Compact,
            false,
            &state1.delta,
            QueryType::General,
            &crate::config::MemFeatureFlags::default(), // E6.4: test uses defaults
        );
        assert_eq!(
            response.freshness, "rebuilt",
            "dirty rebuild should yield freshness=rebuilt"
        );
    }

    #[test]
    fn render_text_includes_freshness_field() {
        let response = MemoryResponse {
            command: "explore".to_string(),
            project_root: "/tmp/p".to_string(),
            project_id: "pid".to_string(),
            artifact_version: ARTIFACT_VERSION,
            detail: DetailLevel::Compact,
            cache_status: CacheStatus::DirtyRebuild,
            cache_hit: false,
            freshness: "rebuilt",
            stats: ProjectStats {
                file_count: 1,
                total_bytes: 1,
                reused_entries: 0,
                rehashed_entries: 1,
                scanned_files: 1,
            },
            delta: None,
            context: ContextSlice {
                entry_points: vec![],
                hot_paths: vec![],
                top_imports: vec![],
                api_surface: vec![],
                module_index: vec![],
                type_graph: vec![], // L2
                dep_manifest: None,
                test_map: vec![],
            },
            graph: GraphSummary { nodes: 1, edges: 0 },
        };
        let text = render_text(&response);
        assert!(
            text.contains("freshness=rebuilt"),
            "text output must contain freshness field"
        );
        assert!(
            text.contains("cache=dirty_rebuild"),
            "text output must show dirty_rebuild status"
        );
    }

    // ── P1: with_retry tests ─────────────────────────────────────────────────

    #[test]
    fn retry_succeeds_on_first_attempt() {
        let result = cache::with_retry(3, || Ok::<u32, anyhow::Error>(42));
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn retry_propagates_non_busy_error_immediately() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let attempts = AtomicU32::new(0);
        let result = cache::with_retry(3, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            Err::<(), _>(anyhow::anyhow!("some other error"))
        });
        assert!(result.is_err());
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "non-busy error should not retry"
        );
    }

    #[test]
    fn retry_retries_on_database_locked() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let attempts = AtomicU32::new(0);
        let result = cache::with_retry(2, || {
            let n = attempts.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                Err::<u32, _>(anyhow::anyhow!("database is locked"))
            } else {
                Ok(99)
            }
        });
        assert_eq!(result.unwrap(), 99);
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            3,
            "should retry twice then succeed"
        );
    }

    // ── Task #10: L2 type extraction tests ───────────────────────────────────

    #[test]
    fn l2_rust_impl_trait_for_struct() {
        // L2: impl Display for MyError → TypeRelation { source: "MyError", target: "Display", relation: "implements" }
        // Note: regex matches single-word trait names only (not qualified paths like std::fmt::Display)
        use super::extractor::extract_type_relations;
        let src = "impl Display for MyError { fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { write!(f, \"err\") } }";
        let relations = extract_type_relations(src, "rust", "src/error.rs");
        let found = relations
            .iter()
            .any(|r| r.source == "MyError" && r.target == "Display" && r.relation == "implements");
        assert!(
            found,
            "expected impl Display for MyError; got: {:?}",
            relations
        );
    }

    #[test]
    fn l2_rust_struct_field_type_relation() {
        // L2: struct Foo { bar: Bar } → "Foo contains Bar"
        use super::extractor::extract_type_relations;
        let src = "struct Foo {\n    bar: Bar,\n    count: u32,\n}";
        let relations = extract_type_relations(src, "rust", "src/foo.rs");
        let found = relations
            .iter()
            .any(|r| r.source == "Foo" && r.target == "Bar" && r.relation == "contains");
        assert!(found, "expected Foo contains Bar; got: {:?}", relations);
        // primitives must not appear
        assert!(
            !relations.iter().any(|r| r.target == "u32"),
            "u32 is a primitive, should be filtered"
        );
    }

    // ── Task #10: artifact_edges roundtrip ───────────────────────────────────

    #[test]
    fn artifact_edges_store_and_get_dependents() {
        // E3.2: store edges, query dependents, strip project_id prefix correctly.
        // Uses a unique project_id to avoid interference with parallel tests.
        use super::cache::{get_dependents, store_artifact_edges};

        let project_id = "test_task10_edges_v1";
        let edges = vec![
            ("src/main.rs".to_string(), "crate::cache".to_string()),
            ("src/util.rs".to_string(), "crate::cache".to_string()),
            (
                "src/other.rs".to_string(),
                "std::collections::HashMap".to_string(),
            ),
        ];

        store_artifact_edges(project_id, &edges).expect("store edges");

        let dependents = get_dependents(project_id, "crate::cache").expect("get_dependents");
        assert!(
            dependents.contains(&"src/main.rs".to_string()),
            "main.rs imports crate::cache; got: {:?}",
            dependents
        );
        assert!(
            dependents.contains(&"src/util.rs".to_string()),
            "util.rs imports crate::cache; got: {:?}",
            dependents
        );
        assert!(
            !dependents.contains(&"src/other.rs".to_string()),
            "other.rs imports HashMap not crate::cache"
        );
    }

    // ── Task #10: cache_stats roundtrip ─────────────────────────────────────

    #[test]
    fn cache_stats_record_and_query_roundtrip() {
        // E1.4: record hit/miss events, verify aggregate query.
        use super::cache::{query_cache_stats, record_cache_event};

        let project_id = "test_task10_stats_v1";
        record_cache_event(project_id, "hit").unwrap();
        record_cache_event(project_id, "hit").unwrap();
        record_cache_event(project_id, "miss").unwrap();

        let stats = query_cache_stats(project_id).unwrap();
        // Use >= comparisons: test may run multiple times (CI re-runs), counts accumulate
        let hit_count = stats
            .iter()
            .find(|(ev, _)| ev == "hit")
            .map(|(_, cnt)| *cnt)
            .unwrap_or(0);
        let miss_count = stats
            .iter()
            .find(|(ev, _)| ev == "miss")
            .map(|(_, cnt)| *cnt)
            .unwrap_or(0);

        assert!(hit_count >= 2, "expected >= 2 hit events, got {hit_count}");
        assert!(
            miss_count >= 1,
            "expected >= 1 miss event, got {miss_count}"
        );
    }

    // ── E1.4: cache_status_event_label ───────────────────────────────────────

    #[test]
    fn cache_status_event_label_returns_correct_labels() {
        // E1.4: verify event labels for common build state combinations
        let base_artifact = ProjectArtifact {
            version: ARTIFACT_VERSION,
            project_id: "p".to_string(),
            project_root: "/tmp".to_string(),
            created_at: 0,
            updated_at: 0,
            file_count: 0,
            total_bytes: 0,
            files: vec![],
            dep_manifest: None,
        };
        let base_stats = ScanStats {
            scanned_files: 1,
            reused_entries: 1,
            rehashed_entries: 0,
        };
        let make_state =
            |previous_exists: bool, stale: bool, cache_hit: bool, changes: usize| BuildState {
                project_root: std::path::PathBuf::from("/tmp"),
                project_id: "p".to_string(),
                previous_exists,
                stale_previous: stale,
                cache_hit,
                scan_stats: base_stats.clone(),
                artifact: base_artifact.clone(),
                delta: indexer::summarize_delta(
                    (0..changes)
                        .map(|i| FileDelta {
                            path: format!("f{i}.rs"),
                            change: DeltaKind::Modified,
                            old_hash: None,
                            new_hash: None,
                        })
                        .collect(),
                ),
                graph: GraphSummary { nodes: 0, edges: 0 },
            };

        assert_eq!(
            cache_status_event_label(&make_state(true, false, true, 0), false),
            "hit"
        );
        assert_eq!(
            cache_status_event_label(&make_state(false, false, false, 0), false),
            "miss"
        );
        assert_eq!(
            cache_status_event_label(&make_state(true, true, false, 0), false),
            "stale_rebuild"
        );
        assert_eq!(
            cache_status_event_label(&make_state(true, false, false, 1), false),
            "dirty_rebuild"
        );
        assert_eq!(
            cache_status_event_label(&make_state(true, false, true, 0), true),
            "refreshed"
        );
    }

    // ── P1: Strict dirty-blocking tests ──────────────────────────────────────

    #[test]
    fn strict_explore_rejects_stale_artifact() {
        // P1: --strict must return Err when artifact TTL has expired (PRD §8)
        use super::cache::{epoch_secs, store_artifact};
        use super::indexer::build_state;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn f() {}").unwrap();

        // Build and store a stale artifact (updated_at set far in the past)
        let mut state = build_state(dir.path(), false, true, 0).expect("build_state");
        store_artifact(&state.artifact).expect("store");

        // Manually age the artifact beyond TTL by setting updated_at = 0
        state.artifact.updated_at = 0;
        store_artifact(&state.artifact).expect("re-store aged artifact");

        // --strict must reject the stale artifact
        let result = run_explore(
            dir.path(),
            false,
            true,
            DetailLevel::Compact,
            "text",
            QueryType::General,
            0,
        );
        assert!(result.is_err(), "strict mode must reject STALE artifact");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("STALE"),
            "error must mention STALE; got: {msg}"
        );
    }

    // ── E6.1: Chaos / concurrent-access tests ────────────────────────────────

    #[test]
    fn chaos_concurrent_store_no_corruption() {
        // E6.1: 8 threads each store+load their own artifact simultaneously.
        // Verifies WAL mode + busy_timeout prevents data corruption under contention.
        use std::thread;

        let handles: Vec<_> = (0..8)
            .map(|i| {
                thread::spawn(move || {
                    let fake_root = format!("/tmp/rtk_chaos_{}", i);
                    let project_id = cache::project_cache_key(std::path::Path::new(&fake_root));
                    let artifact = ProjectArtifact {
                        version: ARTIFACT_VERSION,
                        project_id: project_id.clone(),
                        project_root: fake_root.clone(),
                        created_at: i as u64,
                        updated_at: i as u64,
                        file_count: i,
                        total_bytes: (i * 1024) as u64,
                        files: vec![],
                        dep_manifest: None,
                    };
                    // Store twice to exercise idempotency under contention
                    store_artifact(&artifact).expect("first store");
                    store_artifact(&artifact).expect("second store (idempotent)");

                    let loaded =
                        load_artifact(std::path::Path::new(&fake_root)).expect("load after store");
                    let a = loaded.expect("artifact must be present");
                    assert_eq!(
                        a.file_count, i,
                        "thread {} loaded wrong artifact (file_count mismatch)",
                        i
                    );
                    assert_eq!(
                        a.project_id, project_id,
                        "thread {} loaded wrong project_id",
                        i
                    );
                })
            })
            .collect();

        for h in handles {
            h.join().expect("chaos thread panicked");
        }
    }

    #[test]
    fn chaos_concurrent_store_and_delete() {
        // E6.1: Concurrent store + delete — no panic, result is either present or absent.
        use std::path::PathBuf;
        use std::sync::{Arc, Barrier};
        use std::thread;

        let barrier = Arc::new(Barrier::new(4));
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let b = Arc::clone(&barrier);
                thread::spawn(move || {
                    let fake_root = format!("/tmp/rtk_chaos_del_{}", i % 2); // 2 shared roots
                    let path = PathBuf::from(&fake_root);
                    let artifact = ProjectArtifact {
                        version: ARTIFACT_VERSION,
                        project_id: cache::project_cache_key(&path),
                        project_root: fake_root.clone(),
                        created_at: 0,
                        updated_at: 0,
                        file_count: i,
                        total_bytes: 0,
                        files: vec![],
                        dep_manifest: None,
                    };
                    b.wait(); // synchronised start
                    if i % 2 == 0 {
                        let _ = store_artifact(&artifact); // may race with delete — ok
                    } else {
                        let _ = delete_artifact(&path); // may race with store — ok
                    }
                    // No panic = success; we don't assert the final state (race outcome)
                })
            })
            .collect();

        for h in handles {
            h.join().expect("chaos delete thread panicked");
        }
    }

    // ── E6.2: Cache-hit latency (p95 < 200ms) ────────────────────────────────

    #[test]
    fn cache_hit_latency_p95_under_200ms() {
        // E6.2: Measure repeated cache-hit build_state on a real temp project.
        // p95 target < 200ms per PRD §11. Uses soft assertion: warns on CI slow runs
        // (threshold relaxed to 2000ms to avoid spurious failures on loaded machines).
        use super::indexer::build_state;
        use std::time::Instant;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        for i in 0..20 {
            let sub = dir.path().join(format!("src/module_{i}"));
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::write(
                sub.join("mod.rs"),
                format!(
                    "use crate::module_0::Foo;\npub struct M{i} {{ val: u32 }}\npub fn run() {{}}",
                    i = i
                ),
            )
            .unwrap();
        }
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname=\"bench\"\nversion=\"0.1.0\"",
        )
        .unwrap();

        // Warm-up: one full index + store to populate SQLite cache
        let warmup = build_state(dir.path(), false, true, 0).expect("warm-up explore");
        store_artifact(&warmup.artifact).expect("warm-up store");

        // Benchmark: 30 cache-hit iterations for stable p95
        const RUNS: usize = 30;
        let mut durations_ms: Vec<u64> = Vec::with_capacity(RUNS);
        for _ in 0..RUNS {
            let t = Instant::now();
            let state = build_state(dir.path(), false, true, 0).expect("bench build_state");
            let elapsed = t.elapsed().as_millis() as u64;
            assert!(state.cache_hit, "expected cache hit on repeated calls");
            durations_ms.push(elapsed);
        }

        durations_ms.sort_unstable();
        let p95_idx = ((RUNS as f64 * 0.95) as usize).min(RUNS - 1);
        let p95_ms = durations_ms[p95_idx];
        let p50_ms = durations_ms[RUNS / 2];

        // Hard gate: p95 < 2000ms (catches regressions; even on slow CI this should pass)
        assert!(
            p95_ms < 2000,
            "cache-hit p95 = {}ms, hard gate is 2000ms (PRD target is 200ms)",
            p95_ms
        );
        // Soft warn for PRD target
        if p95_ms >= 200 {
            eprintln!(
                "WARN: cache-hit p95={}ms p50={}ms exceeds PRD target 200ms (environment may be slow)",
                p95_ms, p50_ms
            );
        }
    }

    // ── E4.3: JSON contract tests ─────────────────────────────────────────────

    #[test]
    fn json_response_contains_required_top_level_fields() {
        // E4.3: Serialised MemoryResponse must include all PRD §10.2 required fields.
        let response = MemoryResponse {
            command: "explore".to_string(),
            project_root: "/tmp/proj".to_string(),
            project_id: "deadbeef00000000".to_string(),
            artifact_version: ARTIFACT_VERSION,
            detail: DetailLevel::Compact,
            cache_status: CacheStatus::Hit,
            cache_hit: true,
            freshness: "fresh",
            stats: ProjectStats {
                file_count: 1,
                total_bytes: 512,
                reused_entries: 1,
                rehashed_entries: 0,
                scanned_files: 1,
            },
            delta: None,
            context: ContextSlice {
                entry_points: vec![],
                hot_paths: vec![],
                top_imports: vec![],
                api_surface: vec![],
                module_index: vec![],
                type_graph: vec![],
                dep_manifest: None,
                test_map: vec![],
            },
            graph: GraphSummary { nodes: 1, edges: 0 },
        };

        let json = serde_json::to_value(&response).expect("serialize to JSON");

        // Required top-level fields from PRD §10.2
        for field in &[
            "command",
            "project_root",
            "project_id",
            "artifact_version",
            "cache_status",
            "cache_hit",
            "freshness",
            "stats",
            "context",
            "graph",
        ] {
            assert!(
                json.get(field).is_some(),
                "missing required JSON field: {field}"
            );
        }

        // delta must be absent when None (skip_serializing_if)
        assert!(
            json.get("delta").is_none(),
            "delta must be omitted when None"
        );
    }

    #[test]
    fn json_cache_status_serialises_as_snake_case() {
        // E4.3: CacheStatus enum must serialise as snake_case strings per PRD contract.
        let hit_json = serde_json::to_value(CacheStatus::Hit).unwrap();
        let miss_json = serde_json::to_value(CacheStatus::Miss).unwrap();
        let dirty_json = serde_json::to_value(CacheStatus::DirtyRebuild).unwrap();
        let stale_json = serde_json::to_value(CacheStatus::StaleRebuild).unwrap();
        let refreshed_json = serde_json::to_value(CacheStatus::Refreshed).unwrap();

        assert_eq!(hit_json, serde_json::json!("hit"));
        assert_eq!(miss_json, serde_json::json!("miss"));
        assert_eq!(dirty_json, serde_json::json!("dirty_rebuild"));
        assert_eq!(stale_json, serde_json::json!("stale_rebuild"));
        assert_eq!(refreshed_json, serde_json::json!("refreshed"));
    }

    #[test]
    fn json_delta_present_when_some() {
        // E4.3: delta field must appear in JSON when present, with correct structure.
        let response = MemoryResponse {
            command: "delta".to_string(),
            project_root: "/tmp/proj".to_string(),
            project_id: "aabbccdd00000000".to_string(),
            artifact_version: ARTIFACT_VERSION,
            detail: DetailLevel::Compact,
            cache_status: CacheStatus::Miss,
            cache_hit: false,
            freshness: "rebuilt",
            stats: ProjectStats {
                file_count: 2,
                total_bytes: 1024,
                reused_entries: 0,
                rehashed_entries: 2,
                scanned_files: 2,
            },
            delta: Some(DeltaPayload {
                added: 1,
                modified: 0,
                removed: 0,
                files: vec![FileDelta {
                    path: "src/new.rs".to_string(),
                    change: DeltaKind::Added,
                    old_hash: None,
                    new_hash: Some("aabbccdd".to_string()),
                }],
            }),
            context: ContextSlice {
                entry_points: vec![],
                hot_paths: vec![],
                top_imports: vec![],
                api_surface: vec![],
                module_index: vec![],
                type_graph: vec![],
                dep_manifest: None,
                test_map: vec![],
            },
            graph: GraphSummary { nodes: 2, edges: 0 },
        };

        let json = serde_json::to_value(&response).expect("serialize");
        let delta = json.get("delta").expect("delta must be present");
        assert_eq!(delta.get("added").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(delta.get("modified").and_then(|v| v.as_u64()), Some(0));
        assert_eq!(delta.get("removed").and_then(|v| v.as_u64()), Some(0));
        let files = delta.get("files").and_then(|v| v.as_array()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0].get("path").and_then(|v| v.as_str()),
            Some("src/new.rs")
        );
    }
}

// E0.1: Cache persistence — SQLite WAL backend (migrated from JSON files, ARTIFACT_VERSION 4)
use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use xxhash_rust::xxh3::{xxh3_64, Xxh3};

use super::{ProjectArtifact, ARTIFACT_VERSION};

// ── Path helpers ────────────────────────────────────────────────────────────

pub(super) fn canonical_project_root(project: &Path) -> Result<PathBuf> {
    let canonical = fs::canonicalize(project)
        .with_context(|| format!("Failed to resolve project path {}", project.display()))?;

    if !canonical.is_dir() {
        bail!("Project path must be a directory: {}", canonical.display());
    }

    Ok(canonical)
}

pub(super) fn project_cache_key(project_root: &Path) -> String {
    format!(
        "{:016x}",
        xxh3_64(project_root.to_string_lossy().as_bytes())
    )
}

// ── SQLite helpers ───────────────────────────────────────────────────────────

/// Thread-local DB path override — set by `isolated_db()` in tests.
/// Takes priority over `RTK_MEM_DB_PATH` env var, invisible to other threads.
#[cfg(test)]
thread_local! {
    pub(crate) static THREAD_DB_PATH: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Returns the path to the shared memory-layer SQLite database.
/// Priority: test thread-local > RTK_MEM_DB_PATH env var > default location.
pub(super) fn mem_db_path() -> PathBuf {
    #[cfg(test)]
    {
        let local = THREAD_DB_PATH.with(|p| p.borrow().clone());
        if let Some(path) = local {
            return path;
        }
    }
    if let Ok(p) = std::env::var("RTK_MEM_DB_PATH") {
        return PathBuf::from(p);
    }
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("rtk")
        .join("mem.db")
}

/// Open (or create) the mem.db, enable WAL mode and initialise the schema.
pub(super) fn open_mem_db() -> Result<Connection> {
    let path = mem_db_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create mem.db directory {}", parent.display()))?;
    }
    let conn = Connection::open(&path)
        .with_context(|| format!("Failed to open mem.db at {}", path.display()))?;
    configure_connection(&conn)?;
    init_schema(&conn)?;
    Ok(conn)
}

fn configure_connection(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA busy_timeout=2500;",
    )
    .context("Failed to configure mem.db connection")?;
    Ok(())
}

fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS projects (
            project_id       TEXT    PRIMARY KEY,
            root_path        TEXT    NOT NULL UNIQUE,
            created_at       INTEGER NOT NULL,
            last_accessed_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS artifacts (
            project_id       TEXT    PRIMARY KEY,
            artifact_version INTEGER NOT NULL,
            content_json     TEXT    NOT NULL,
            updated_at       INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS cache_stats (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            project_id TEXT    NOT NULL,
            event      TEXT    NOT NULL,
            timestamp  INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS artifact_edges (
            from_id   TEXT,
            to_id     TEXT,
            edge_type TEXT,
            PRIMARY KEY (from_id, to_id, edge_type)
         );
         CREATE TABLE IF NOT EXISTS events (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            project_id  TEXT    NOT NULL,
            event_type  TEXT    NOT NULL,
            timestamp   INTEGER NOT NULL,
            duration_ms INTEGER
         );
         CREATE INDEX IF NOT EXISTS idx_projects_accessed
             ON projects(last_accessed_at);
         CREATE INDEX IF NOT EXISTS idx_events_project
             ON events(project_id, event_type);
         CREATE INDEX IF NOT EXISTS idx_artifacts_version
             ON artifacts(project_id, artifact_version);
         CREATE TABLE IF NOT EXISTS episodes (
            session_id       TEXT    PRIMARY KEY,
            project_id       TEXT    NOT NULL,
            task_text        TEXT    NOT NULL,
            task_fingerprint TEXT,
            query_type       TEXT,
            started_at       INTEGER NOT NULL,
            ended_at         INTEGER,
            outcome          TEXT,
            token_budget     INTEGER,
            token_used       INTEGER,
            latency_ms       INTEGER
         );
         CREATE TABLE IF NOT EXISTS episode_events (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id   TEXT    NOT NULL,
            event_type   TEXT    NOT NULL,
            file_path    TEXT,
            symbol       TEXT,
            payload_json TEXT,
            timestamp    INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS causal_links (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id   TEXT    NOT NULL,
            issue_ref    TEXT,
            commit_sha   TEXT,
            change_path  TEXT    NOT NULL,
            change_kind  TEXT    NOT NULL,
            rationale    TEXT,
            timestamp    INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_episodes_project
             ON episodes(project_id, started_at);
         CREATE INDEX IF NOT EXISTS idx_episode_events_session
             ON episode_events(session_id);",
    )
    .context("Failed to initialise mem.db schema")?;
    Ok(())
}

// ── Artifact I/O ─────────────────────────────────────────────────────────────

pub(super) fn load_artifact(project_root: &Path) -> Result<Option<ProjectArtifact>> {
    let project_id = project_cache_key(project_root);
    let conn = open_mem_db()?;

    let row: Option<(String, u32)> = conn
        .query_row(
            "SELECT content_json, artifact_version FROM artifacts WHERE project_id = ?1",
            params![project_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .context("Failed to query artifact from mem.db")?;

    let Some((content_json, version)) = row else {
        return Ok(None);
    };

    // Bump last_accessed_at on every successful load
    let now = epoch_secs(SystemTime::now()) as i64;
    let _ = conn.execute(
        "UPDATE projects SET last_accessed_at = ?1 WHERE project_id = ?2",
        params![now, project_id],
    );

    if version != ARTIFACT_VERSION {
        return Ok(None); // stale schema — caller will trigger full rebuild
    }

    let artifact: ProjectArtifact =
        serde_json::from_str(&content_json).context("Failed to parse artifact JSON from mem.db")?;
    Ok(Some(artifact))
}

pub(super) fn store_artifact(artifact: &ProjectArtifact) -> Result<()> {
    // P1: retry on transient SQLITE_BUSY for multi-agent concurrency (PRD §1.2)
    with_retry(3, || store_artifact_inner(artifact))
}

fn store_artifact_inner(artifact: &ProjectArtifact) -> Result<()> {
    let conn = open_mem_db()?;
    let now = epoch_secs(SystemTime::now()) as i64;
    let content_json =
        serde_json::to_string(artifact).context("Failed to serialise artifact to JSON")?;

    conn.execute(
        "INSERT OR REPLACE INTO projects
             (project_id, root_path, created_at, last_accessed_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![artifact.project_id, artifact.project_root, now, now],
    )
    .context("Failed to upsert project in mem.db")?;

    conn.execute(
        "INSERT OR REPLACE INTO artifacts
             (project_id, artifact_version, content_json, updated_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![artifact.project_id, ARTIFACT_VERSION, content_json, now],
    )
    .context("Failed to upsert artifact in mem.db")?;

    prune_cache(&conn, super::mem_config().cache_max_projects)?;
    Ok(())
}

/// Delete cached artifact for `project_root`. Returns true if a row was removed.
pub(super) fn delete_artifact(project_root: &Path) -> Result<bool> {
    let project_root = project_root.to_path_buf();
    // P1: retry on transient SQLITE_BUSY (PRD §1.2)
    with_retry(3, || delete_artifact_inner(&project_root))
}

fn delete_artifact_inner(project_root: &Path) -> Result<bool> {
    let project_id = project_cache_key(project_root);
    let conn = open_mem_db()?;

    let deleted = conn
        .execute(
            "DELETE FROM artifacts WHERE project_id = ?1",
            params![project_id],
        )
        .context("Failed to delete artifact from mem.db")?;

    conn.execute(
        "DELETE FROM projects WHERE project_id = ?1",
        params![project_id],
    )
    .context("Failed to delete project from mem.db")?;

    Ok(deleted > 0)
}

/// P1: Retry wrapper for SQLite operations that may fail with SQLITE_BUSY
/// under concurrent multi-agent access. Retries up to `max_retries` times
/// with exponential backoff (100ms, 200ms, 400ms).
pub(super) fn with_retry<T, F: Fn() -> Result<T>>(max_retries: u32, op: F) -> Result<T> {
    let mut attempt = 0;
    loop {
        match op() {
            Ok(val) => return Ok(val),
            Err(e) => {
                let is_busy = e
                    .chain()
                    .any(|cause| cause.to_string().contains("database is locked"));
                if !is_busy || attempt >= max_retries {
                    return Err(e);
                }
                attempt += 1;
                let backoff_ms = 100 * (1u64 << (attempt - 1)); // 100, 200, 400ms
                std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
            }
        }
    }
}

fn prune_cache(conn: &Connection, max_projects: usize) -> Result<()> {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))
        .unwrap_or(0);

    if count <= max_projects as i64 {
        return Ok(());
    }

    let remove_count = count - max_projects as i64;
    conn.execute(
        "DELETE FROM projects WHERE project_id IN
             (SELECT project_id FROM projects ORDER BY last_accessed_at ASC LIMIT ?1)",
        params![remove_count],
    )
    .context("Failed to prune old projects from mem.db")?;

    conn.execute(
        "DELETE FROM artifacts
         WHERE project_id NOT IN (SELECT project_id FROM projects)",
        [],
    )
    .context("Failed to prune orphaned artifacts from mem.db")?;

    Ok(())
}

// ── E1.4: cache_stats metrics ────────────────────────────────────────────────

/// Record a cache event (hit, miss, stale_rebuild, dirty_rebuild, refreshed) for analytics.
pub(super) fn record_cache_event(project_id: &str, event: &str) -> Result<()> {
    let conn = open_mem_db()?;
    let now = epoch_secs(SystemTime::now()) as i64;
    conn.execute(
        "INSERT INTO cache_stats (project_id, event, timestamp) VALUES (?1, ?2, ?3)",
        params![project_id, event, now],
    )
    .context("Failed to record cache_stats event")?;
    Ok(())
}

/// Query aggregate cache_stats for a project. Returns Vec<(event, count)>.
pub(super) fn query_cache_stats(project_id: &str) -> Result<Vec<(String, i64)>> {
    let conn = open_mem_db()?;
    let mut stmt = conn
        .prepare(
            "SELECT event, COUNT(*) as cnt FROM cache_stats
             WHERE project_id = ?1 GROUP BY event ORDER BY cnt DESC",
        )
        .context("Failed to prepare cache_stats query")?;
    let rows = stmt
        .query_map(params![project_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .context("Failed to query cache_stats")?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row.context("Failed to read cache_stats row")?);
    }
    Ok(result)
}

// ── E3.2: artifact_edges for cascade invalidation ───────────────────────────

/// Store import edges for a project: from_id = importing file, to_id = imported module.
/// Clears previous edges for the project before inserting new ones.
pub(super) fn store_artifact_edges(
    project_id: &str,
    edges: &[(String, String)], // (from_file, to_module)
) -> Result<()> {
    let conn = open_mem_db()?;
    // Clear old edges for this project
    conn.execute(
        "DELETE FROM artifact_edges WHERE from_id LIKE ?1",
        params![format!("{}:%", project_id)],
    )
    .context("Failed to clear old artifact_edges")?;

    // Insert new edges (prefix from_id with project_id for namespacing)
    let mut stmt = conn
        .prepare(
            "INSERT OR IGNORE INTO artifact_edges (from_id, to_id, edge_type)
             VALUES (?1, ?2, 'imports')",
        )
        .context("Failed to prepare artifact_edges insert")?;

    for (from_file, to_module) in edges {
        let from_key = format!("{}:{}", project_id, from_file);
        let _ = stmt.execute(params![from_key, to_module]);
    }
    Ok(())
}

/// Find files that import a given module (for cascade invalidation).
/// Returns the rel_path portion of from_id entries that import `module_name`.
pub(super) fn get_dependents(project_id: &str, module_name: &str) -> Result<Vec<String>> {
    let conn = open_mem_db()?;
    let prefix = format!("{}:", project_id);
    let mut stmt = conn
        .prepare(
            "SELECT from_id FROM artifact_edges
             WHERE to_id = ?1 AND from_id LIKE ?2",
        )
        .context("Failed to prepare dependents query")?;

    let rows = stmt
        .query_map(params![module_name, format!("{}%", prefix)], |row| {
            row.get::<_, String>(0)
        })
        .context("Failed to query dependents")?;

    let mut result = Vec::new();
    for row in rows {
        let from_id = row.context("Failed to read dependent row")?;
        // Strip project_id: prefix to get rel_path
        if let Some(rel) = from_id.strip_prefix(&prefix) {
            result.push(rel.to_string());
        }
    }
    Ok(result)
}

// ── Staleness check ──────────────────────────────────────────────────────────

pub(super) fn is_artifact_stale(artifact: &ProjectArtifact) -> bool {
    let now = epoch_secs(SystemTime::now());
    // Use configurable TTL; falls back to 86400s default when no config file exists
    now.saturating_sub(artifact.updated_at) > super::mem_config().cache_ttl_secs
}

// ── File hashing ─────────────────────────────────────────────────────────────

pub(super) fn hash_file(path: &Path) -> Result<u64> {
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

pub(super) fn format_hash(value: u64) -> String {
    format!("{:016x}", value)
}

// ── E4.1/PRD §9: events table ────────────────────────────────────────────────

/// Record a lifecycle event (explore/delta/refresh/watch/api) with optional duration.
pub(super) fn record_event(
    project_id: &str,
    event_type: &str,
    duration_ms: Option<u64>,
) -> Result<()> {
    let conn = open_mem_db()?;
    let now = epoch_secs(SystemTime::now()) as i64;
    conn.execute(
        "INSERT INTO events (project_id, event_type, timestamp, duration_ms)
         VALUES (?1, ?2, ?3, ?4)",
        params![project_id, event_type, now, duration_ms.map(|d| d as i64)],
    )
    .context("Failed to record event")?;
    Ok(())
}

// ── Timestamp helpers ─────────────────────────────────────────────────────────

pub(super) fn epoch_secs(ts: SystemTime) -> u64 {
    ts.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(super) fn epoch_nanos(ts: SystemTime) -> u128 {
    ts.duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

// ── Test isolation ────────────────────────────────────────────────────────────

/// Global mutex for tests that mutate RTK_MEM_DB_PATH.
/// env::set_var is not thread-safe; this serializes all env-var-touching tests.
#[cfg(test)]
pub(crate) static TEST_DB_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

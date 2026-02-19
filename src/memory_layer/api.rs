// E4.1: Minimal HTTP/1.1 API server — localhost only, no async deps required.
// Endpoints: GET /v1/health, POST /v1/{explore,delta,refresh,context}
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock}; // P1: OnceLock for CG_CACHE
use std::time::{Duration, Instant};

use super::cache::record_event;
use super::indexer::build_git_delta;
use super::renderer::{build_response, render_text};
use super::{budget, git_churn, intent, ranker}; // plan-context pipeline
use super::{indexer, DetailLevel, QueryType, ARTIFACT_VERSION};
use super::{record_cache_event, store_artifact, store_import_edges};

// Keep poll sleep small to avoid adding ~50ms queueing delay to localhost requests.
const ACCEPT_POLL_SLEEP: Duration = Duration::from_millis(5);

// P1: in-process call graph cache: project_id → Arc<CallGraph>.
// Invalidated when the artifact is rebuilt (!state.cache_hit).
static CG_CACHE: OnceLock<Mutex<std::collections::HashMap<String, Arc<super::call_graph::CallGraph>>>> =
    OnceLock::new();

fn cg_cache_global(
) -> &'static Mutex<std::collections::HashMap<String, Arc<super::call_graph::CallGraph>>> {
    CG_CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

// ── Request / response structs ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ApiRequest {
    project_root: String,
    #[serde(default)]
    query_type: QueryType,
    #[serde(default)]
    detail: DetailLevel,
    /// For /v1/delta: git ref (e.g. "HEAD~5") or None for FS delta
    since: Option<String>,
    /// "json" (default) or "text"
    #[serde(default = "default_format")]
    format: String,
}

fn default_format() -> String {
    "json".to_string()
}

// ── Plan-context request ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct PlanRequest {
    project_root: String,
    /// Task description (e.g. "fix jwt token refresh bug")
    #[serde(default)]
    task: String,
    /// Max tokens to include. 0 = use default (4000).
    #[serde(default)]
    token_budget: u32,
    /// Output format: "json" or "text"
    #[serde(default = "default_format")]
    format: String,
    /// Enable Ollama Stage-2 rerank: "off" (default) or "full"
    #[serde(default)]
    ml_mode: MlMode,
}

#[derive(Debug, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MlMode {
    #[default]
    Off,
    Full,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
    artifact_version: u32,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

// ── PID file lifecycle guard ──────────────────────────────────────────────────

struct PidGuard {
    path: PathBuf,
}

impl PidGuard {
    fn write(port: u16) -> Self {
        let path = dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("rtk")
            .join(format!("mem-server-{port}.pid"));
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, std::process::id().to_string());
        Self { path }
    }
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// ── HTTP parsing helpers ──────────────────────────────────────────────────────

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn parse_request(stream: &TcpStream) -> Result<Request> {
    let mut reader = BufReader::new(stream);

    // Read request line
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .context("Failed to read request line")?;
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        bail!("Malformed request line: {}", request_line.trim());
    }
    let method = parts[0].to_uppercase();
    let path = parts[1].to_string();

    // Read headers
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .context("Failed to read header")?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        let lower = trimmed.to_lowercase();
        if lower.starts_with("content-length:") {
            content_length = trimmed["content-length:".len()..]
                .trim()
                .parse()
                .unwrap_or(0);
        }
    }

    // Read body
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        use std::io::Read;
        reader
            .read_exact(&mut body)
            .context("Failed to read request body")?;
    }

    Ok(Request { method, path, body })
}

fn write_response(stream: &TcpStream, status: u16, body: &str) -> Result<()> {
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "Unknown",
    };
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        len = body.len(),
    );
    let mut w = stream;
    w.write_all(response.as_bytes())
        .context("Failed to write HTTP response")?;
    Ok(())
}

fn json_error(msg: &str) -> String {
    serde_json::to_string(&ErrorResponse {
        error: msg.to_string(),
    })
    .unwrap_or_else(|_| r#"{"error":"serialization failed"}"#.to_string())
}

// ── Endpoint handlers ─────────────────────────────────────────────────────────

fn handle_health() -> Result<String> {
    serde_json::to_string(&HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        artifact_version: ARTIFACT_VERSION,
    })
    .context("Failed to serialize health response")
}

fn handle_explore(body: &[u8]) -> Result<String> {
    let req: ApiRequest =
        serde_json::from_slice(body).context("Invalid JSON in explore request")?;
    let project = PathBuf::from(&req.project_root);
    let t = Instant::now();
    let cfg = super::mem_config(); // E6.4: read feature flags

    let state = indexer::build_state(&project, false, cfg.features.cascade_invalidation, 0)?; // E6.4
    if !state.cache_hit {
        store_artifact(&state.artifact)?;
        store_import_edges(&state.artifact);
    }
    let _ = record_cache_event(&state.project_id, "hit");
    let duration_ms = t.elapsed().as_millis() as u64;
    let _ = record_event(&state.project_id, "api:explore", Some(duration_ms));

    let response = build_response(
        "explore",
        &state,
        req.detail,
        false,
        &state.delta,
        req.query_type,
        &cfg.features, // E6.4
    );
    if req.format == "text" {
        return Ok(serde_json::json!({ "text": render_text(&response) }).to_string());
    }
    serde_json::to_string(&response).context("Failed to serialize explore response")
}

fn handle_delta(body: &[u8]) -> Result<String> {
    let req: ApiRequest = serde_json::from_slice(body).context("Invalid JSON in delta request")?;
    let project = PathBuf::from(&req.project_root);
    let t = Instant::now();
    let cfg = super::mem_config(); // E6.4: read feature flags

    let state = indexer::build_state(&project, false, cfg.features.cascade_invalidation, 0)?; // E6.4
    if !state.delta.changes.is_empty() {
        store_artifact(&state.artifact)?;
    }
    let response_delta = if let Some(ref rev) = req.since {
        if !cfg.features.git_delta {
            // E6.4: git_delta feature flag guard in API
            bail!(
                "git delta is disabled via [mem.features] git_delta = false. \
                 Omit 'since' to use FS delta."
            );
        }
        build_git_delta(&state.project_root, rev, 0)?
    } else {
        state.delta.clone()
    };
    let _ = record_cache_event(&state.project_id, "delta");
    let duration_ms = t.elapsed().as_millis() as u64;
    let _ = record_event(&state.project_id, "api:delta", Some(duration_ms));

    let response = build_response(
        "delta",
        &state,
        req.detail,
        false,
        &response_delta,
        req.query_type,
        &cfg.features, // E6.4
    );
    serde_json::to_string(&response).context("Failed to serialize delta response")
}

fn handle_refresh(body: &[u8]) -> Result<String> {
    let req: ApiRequest =
        serde_json::from_slice(body).context("Invalid JSON in refresh request")?;
    let project = PathBuf::from(&req.project_root);
    let t = Instant::now();
    let cfg = super::mem_config(); // E6.4: read feature flags

    let state = indexer::build_state(&project, true, cfg.features.cascade_invalidation, 0)?; // E6.4
    store_artifact(&state.artifact)?;
    store_import_edges(&state.artifact);
    let _ = record_cache_event(&state.project_id, "refreshed");
    let duration_ms = t.elapsed().as_millis() as u64;
    let _ = record_event(&state.project_id, "api:refresh", Some(duration_ms));

    let response = build_response(
        "refresh",
        &state,
        req.detail,
        true,
        &state.delta,
        req.query_type,
        &cfg.features, // E6.4
    );
    serde_json::to_string(&response).context("Failed to serialize refresh response")
}

// ── Plan-context handler ─────────────────────────────────────────────────────

fn handle_plan_context(body: &[u8]) -> Result<String> {
    let req: PlanRequest =
        serde_json::from_slice(body).context("Invalid JSON in plan-context request")?;
    let project = std::path::PathBuf::from(&req.project_root);
    let token_budget = if req.token_budget == 0 {
        4000
    } else {
        req.token_budget
    };
    let cfg = super::mem_config();

    // 1. Build/reuse artifact
    let state = indexer::build_state(&project, false, cfg.features.cascade_invalidation, 0)?;
    if !state.cache_hit {
        store_artifact(&state.artifact)?;
        store_import_edges(&state.artifact);
    }

    // 2. Load git churn (cached by HEAD sha)
    let churn =
        git_churn::load_churn(&state.project_root).unwrap_or_else(|_| git_churn::ChurnCache {
            head_sha: "unknown".to_string(),
            freq_map: std::collections::HashMap::new(),
            max_count: 0,
        });

    // 3. Parse intent for weight tuning
    let parsed_intent = intent::parse_intent(&req.task, &state.project_id);

    // 4. Collect recent-change paths for f_recency_score
    let recent_paths: std::collections::HashSet<String> =
        state.delta.changes.iter().map(|d| d.path.clone()).collect();

    // 5a. Build call graph from artifact pub fn symbols
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
    // P1: retrieve cached graph when artifact unchanged; rebuild + store otherwise
    let cached_cg: Option<Arc<super::call_graph::CallGraph>> = if state.cache_hit {
        cg_cache_global()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&state.project_id)
            .cloned()
    } else {
        None
    };
    let cg_arc = match cached_cg {
        Some(cg) => cg,
        None => {
            let built =
                Arc::new(super::call_graph::CallGraph::build(&all_symbols, &state.project_root));
            cg_cache_global()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(state.project_id.clone(), Arc::clone(&built));
            built
        }
    };
    let query_tags = parsed_intent.extracted_tags.clone();

    // 5b. Build candidates from artifact file list
    let candidates: Vec<ranker::Candidate> = state
        .artifact
        .files
        .iter()
        .map(|fa| {
            let mut c = ranker::Candidate::new(&fa.rel_path);

            // f_structural_relevance: files with public symbols score higher
            c.features.f_structural_relevance = if !fa.pub_symbols.is_empty() {
                1.0
            } else if !fa.imports.is_empty() {
                0.5
            } else {
                0.2
            };

            // f_churn_score: objective git history signal
            c.features.f_churn_score = git_churn::churn_score(&churn, &fa.rel_path);

            // f_recency_score: file in current delta
            c.features.f_recency_score = if recent_paths.contains(&fa.rel_path) {
                1.0
            } else {
                0.0
            };

            // f_risk_score: auth/payment/admin path keywords
            c.features.f_risk_score = ranker::path_risk_score(&fa.rel_path);

            // f_test_proximity: is this a test file?
            c.features.f_test_proximity = if ranker::is_test_file(&fa.rel_path) {
                0.8
            } else {
                0.0
            };

            // f_call_graph_score: callers of query-relevant symbols
            c.features.f_call_graph_score = cg_arc.caller_score(&fa.rel_path, &query_tags); // P1: use cached Arc

            // f_token_cost: normalized estimated cost
            let raw_cost = budget::estimate_tokens_for_path(&fa.rel_path);
            c.estimated_tokens = raw_cost;
            c.features.f_token_cost = (raw_cost as f32 / 1000.0).min(1.0);

            c.sources.push("artifact".to_string());
            c
        })
        .collect();

    // 6. Stage-1: deterministic ranking
    let ranked = ranker::rank_stage1(candidates, &parsed_intent);

    // 7. Stage-2: Ollama rerank (only if explicitly requested)
    let ranked = if req.ml_mode == MlMode::Full {
        // Ollama rerank skipped in this path — add when ollama.rs is wired
        ranked
    } else {
        ranked
    };

    // 8. Budget-aware assembly (greedy knapsack)
    let result = budget::assemble(ranked, token_budget);

    serde_json::to_string(&result).context("Failed to serialize plan-context response")
}

// ── Connection handler ────────────────────────────────────────────────────────

fn handle_connection(stream: TcpStream, verbose: u8) {
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();

    let req = match parse_request(&stream) {
        Ok(r) => r,
        Err(e) => {
            if verbose > 0 {
                eprintln!("memory.serve: parse error: {e}");
            }
            let _ = write_response(&stream, 400, &json_error(&e.to_string()));
            return;
        }
    };

    if verbose > 0 {
        eprintln!("memory.serve: {} {}", req.method, req.path);
    }

    let (status, body) = match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/v1/health") => match handle_health() {
            Ok(b) => (200, b),
            Err(e) => (500, json_error(&e.to_string())),
        },
        ("POST", "/v1/explore") | ("POST", "/v1/context") => match handle_explore(&req.body) {
            Ok(b) => (200, b),
            Err(e) => (500, json_error(&e.to_string())),
        },
        ("POST", "/v1/delta") => match handle_delta(&req.body) {
            Ok(b) => (200, b),
            Err(e) => (500, json_error(&e.to_string())),
        },
        ("POST", "/v1/refresh") => match handle_refresh(&req.body) {
            Ok(b) => (200, b),
            Err(e) => (500, json_error(&e.to_string())),
        },
        ("POST", "/v1/plan-context") => match handle_plan_context(&req.body) {
            Ok(b) => (200, b),
            Err(e) => (500, json_error(&e.to_string())),
        },
        ("GET", _) | ("POST", _) => (
            404,
            json_error(
                "Not found. Available: GET /v1/health, POST /v1/{explore,delta,refresh,context,plan-context}",
            ),
        ),
        _ => (405, json_error("Method not allowed")),
    };

    let _ = write_response(&stream, status, &body);
}

// ── Public entry point ────────────────────────────────────────────────────────

/// E4.1: Start HTTP API server on localhost:{port}.
/// Exits after `idle_secs` of no requests (daemon lifecycle).
/// Writes PID file for external lifecycle management.
pub(super) fn serve(port: u16, idle_secs: u64, verbose: u8) -> Result<()> {
    let addr = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&addr).with_context(|| format!("Failed to bind to {addr}"))?;
    listener
        .set_nonblocking(true)
        .context("Failed to set non-blocking mode")?;

    let _pid = PidGuard::write(port);
    let idle_timeout = Duration::from_secs(if idle_secs == 0 { u64::MAX } else { idle_secs });
    let last_request = Arc::new(Mutex::new(Instant::now()));

    eprintln!(
        "memory.serve: listening on http://{addr} (idle-timeout={}s)",
        if idle_secs == 0 {
            "∞".to_string()
        } else {
            idle_secs.to_string()
        }
    );

    loop {
        match listener.accept() {
            Ok((stream, peer)) => {
                *last_request.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now(); // P1: handle poisoned mutex
                if verbose > 0 {
                    eprintln!("memory.serve: connection from {peer}");
                }
                let v = verbose;
                std::thread::spawn(move || handle_connection(stream, v));
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if last_request.lock().unwrap_or_else(|e| e.into_inner()).elapsed() > idle_timeout { // P1: handle poisoned mutex
                    eprintln!("memory.serve: idle timeout ({idle_secs}s), stopping");
                    break;
                }
                std::thread::sleep(ACCEPT_POLL_SLEEP);
            }
            Err(e) => {
                eprintln!("memory.serve: accept error: {e}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    eprintln!("memory.serve: stopped");
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_response_is_valid_json() {
        let body = handle_health().expect("health should succeed");
        let val: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(val["status"], "ok");
        assert!(val["version"].is_string());
        assert!(val["artifact_version"].is_number());
    }

    #[test]
    fn invalid_project_returns_error_json() {
        // Explore a non-existent path must return an Err (not panic)
        let body = serde_json::json!({
            "project_root": "/tmp/rtk_api_nonexistent_xyz_12345"
        })
        .to_string();
        // Should produce Err (no such dir)
        let result = handle_explore(body.as_bytes());
        assert!(result.is_err(), "non-existent project must return Err");
    }

    #[test]
    fn server_binds_and_health_check_works() {
        // E4.1: bind a server on a random port, send GET /v1/health, verify response
        use std::io::{Read, Write};
        use std::net::TcpStream;

        // Bind on port 0 = OS assigns a free port
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        drop(listener); // release so serve() can re-bind — test uses direct connection approach

        // Spawn a minimal ad-hoc server for one request
        let listener2 = TcpListener::bind(format!("127.0.0.1:{port}")).expect("rebind");
        listener2.set_nonblocking(false).ok();
        let handle = std::thread::spawn(move || {
            if let Ok((stream, _)) = listener2.accept() {
                handle_connection(stream, 0);
            }
        });

        // Client sends GET /v1/health
        let mut conn = TcpStream::connect(format!("127.0.0.1:{port}")).expect("connect");
        write!(
            conn,
            "GET /v1/health HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n"
        )
        .expect("write request");
        conn.flush().ok();

        let mut response = String::new();
        conn.read_to_string(&mut response).expect("read response");
        handle.join().ok();

        assert!(
            response.contains("200 OK"),
            "expected 200 OK; got: {response}"
        );
        assert!(
            response.contains("\"status\":\"ok\""),
            "expected health JSON; got: {response}"
        );
    }
}

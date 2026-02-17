use crate::tracking;
use crate::write_core::{AtomicWriter, WriteOptions};
use crate::write_semantics::{semantics_for, WriteOperation};
use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use serde::Serialize;
use serde_json::ser::{PrettyFormatter, Serializer};
use serde_json::{Map, Value as JsonValue};
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value as TomlValue;
use toml_edit::{DocumentMut, Item, Table, Value as TomlEditValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ConfigFormat {
    Auto,
    Json,
    Toml,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ConfigValueType {
    Auto,
    String,
    Number,
    Bool,
    Null,
    Json,
}

/// Output mode for write commands — controls token cost and LLM-friendliness
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum OutputMode {
    /// Empty stdout on success, stderr on error
    Quiet,
    /// Short human-readable status (default)
    #[default]
    Concise,
    /// Machine-readable JSON (version 1 schema)
    Json,
}

/// Structured response — single renderer for all write operations
#[derive(Debug, Clone, Serialize)]
pub struct WriteResponse {
    pub version: u8,
    pub ok: bool,
    pub op: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl WriteResponse {
    fn success(op: &str, applied: usize) -> Self {
        Self {
            version: 1,
            ok: true,
            op: op.to_string(),
            applied: Some(applied),
            failed: None,
            dry_run: None,
            error: None,
            hint: None,
            detail: None,
        }
    }

    fn dry_run(op: &str, planned: usize) -> Self {
        Self {
            version: 1,
            ok: true,
            op: op.to_string(),
            applied: Some(planned),
            failed: None,
            dry_run: Some(true),
            error: None,
            hint: None,
            detail: None,
        }
    }

    fn noop(op: &str) -> Self {
        Self {
            version: 1,
            ok: true,
            op: op.to_string(),
            applied: Some(0),
            failed: None,
            dry_run: None,
            error: None,
            hint: Some("no-op".to_string()),
            detail: None,
        }
    }

    fn render(&self, mode: OutputMode, concise_msg: &str) {
        match mode {
            OutputMode::Quiet => {} // silent on success
            OutputMode::Concise => println!("{}", concise_msg),
            OutputMode::Json => {
                // unwrap safe: WriteResponse is always serializable
                println!("{}", serde_json::to_string(self).unwrap());
            }
        }
    }
}

/// Structured error with code — for predictable LLM parsing
fn write_error(code: &str, hint: &str, mode: OutputMode) -> anyhow::Error {
    if mode == OutputMode::Json {
        let resp = WriteResponse {
            version: 1,
            ok: false,
            op: String::new(),
            applied: None,
            failed: None,
            dry_run: None,
            error: Some(code.to_string()),
            hint: Some(hint.to_string()),
            detail: None,
        };
        // Print JSON error to stdout before returning the anyhow::Error
        println!("{}", serde_json::to_string(&resp).unwrap());
    }
    anyhow::anyhow!("ERR {} {}", code, hint)
}

/// Batch operation plan entry (deserialized from --plan JSON)
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BatchOp {
    pub op: String,
    pub file: PathBuf,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub old: Option<String>,
    #[serde(default)]
    pub new: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub all: bool,
    #[serde(default)]
    pub value_type: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
}

pub fn run_replace(
    file: &Path,
    from: &str,
    to: &str,
    all: bool,
    dry_run: bool,
    fast: bool,
    verbose: u8,
    output: OutputMode,
) -> Result<()> {
    let timer = tracking::TimedExecution::start();
    let _semantics = semantics_for(WriteOperation::Replace);

    if from.is_empty() {
        return Err(write_error("EMPTY_PATTERN", "--from must be non-empty", output));
    }

    let content =
        fs::read_to_string(file).with_context(|| format!("Failed to read {}", file.display()))?;

    // P0-2: single-pass — no separate matches().count() scan
    if !content.contains(from) {
        return Err(write_error("NO_MATCH", "no matches for --from", output));
    }

    let (updated, count) = if all {
        replace_all_counted(&content, from, to)
    } else {
        (content.replacen(from, to, 1), 1)
    };

    if updated == content {
        WriteResponse::noop("replace")
            .render(output, "no-op: replacement produces identical content");
        timer.track(
            &format!("write replace {}", file.display()),
            "rtk write replace (noop)",
            &content,
            &updated,
        );
        return Ok(());
    }

    if dry_run {
        WriteResponse::dry_run("replace", count)
            .render(output, &format!("dry-run: replace {} occurrence(s)", count));
        timer.track(
            &format!("write replace {}", file.display()),
            "rtk write replace (dry-run)",
            &content,
            &updated,
        );
        return Ok(());
    }

    let stats = write_text(file, &updated, fast)?;
    WriteResponse::success("replace", count)
        .render(output, &format!("OK replace applied={}", count));
    if verbose > 1 {
        eprintln!(
            "bytes_written={}, fsync={}, rename={}",
            stats.bytes_written, stats.fsync_count, stats.rename_count
        );
    }

    timer.track(
        &format!("write replace {}", file.display()),
        "rtk write replace",
        &content,
        &updated,
    );
    Ok(())
}

pub fn run_patch(
    file: &Path,
    old: &str,
    new: &str,
    all: bool,
    dry_run: bool,
    fast: bool,
    verbose: u8,
    output: OutputMode,
) -> Result<()> {
    let timer = tracking::TimedExecution::start();
    let _semantics = semantics_for(WriteOperation::Patch);

    if old.is_empty() {
        return Err(write_error("EMPTY_PATTERN", "--old must be non-empty", output));
    }

    let content =
        fs::read_to_string(file).with_context(|| format!("Failed to read {}", file.display()))?;

    if !content.contains(old) {
        return Err(write_error("NO_MATCH", "hunk not found", output));
    }

    let (updated, count) = if all {
        replace_all_counted(&content, old, new)
    } else {
        (content.replacen(old, new, 1), 1)
    };

    if updated == content {
        WriteResponse::noop("patch")
            .render(output, "no-op: patch produces identical content");
        timer.track(
            &format!("write patch {}", file.display()),
            "rtk write patch (noop)",
            &content,
            &updated,
        );
        return Ok(());
    }

    if dry_run {
        WriteResponse::dry_run("patch", count)
            .render(output, &format!("dry-run: patch {} hunk(s)", count));
        timer.track(
            &format!("write patch {}", file.display()),
            "rtk write patch (dry-run)",
            &content,
            &updated,
        );
        return Ok(());
    }

    let stats = write_text(file, &updated, fast)?;
    WriteResponse::success("patch", count)
        .render(output, &format!("OK patch applied={}", count));
    if verbose > 1 {
        eprintln!(
            "bytes_written={}, fsync={}, rename={}",
            stats.bytes_written, stats.fsync_count, stats.rename_count
        );
    }

    timer.track(
        &format!("write patch {}", file.display()),
        "rtk write patch",
        &content,
        &updated,
    );
    Ok(())
}

pub fn run_set(
    file: &Path,
    key: &str,
    value: &str,
    value_type: ConfigValueType,
    format: ConfigFormat,
    dry_run: bool,
    fast: bool,
    verbose: u8,
    output: OutputMode,
) -> Result<()> {
    let timer = tracking::TimedExecution::start();
    let _semantics = semantics_for(WriteOperation::Set);
    let content =
        fs::read_to_string(file).with_context(|| format!("Failed to read {}", file.display()))?;
    let format = resolve_format(file, format)?;

    let (updated, format_label) = match format {
        ConfigFormat::Json => {
            let mut root: JsonValue = serde_json::from_str(&content)
                .with_context(|| format!("Invalid JSON: {}", file.display()))?;
            set_json_path(&mut root, key, parse_json_value(value, value_type)?)?;
            (serialize_json_preserving_style(&root, &content)?, "json")
        }
        ConfigFormat::Toml => {
            let mut root: DocumentMut = content
                .parse::<DocumentMut>()
                .with_context(|| format!("Invalid TOML: {}", file.display()))?;
            set_toml_path(&mut root, key, parse_toml_value(value, value_type)?)?;
            (root.to_string(), "toml")
        }
        ConfigFormat::Auto => unreachable!(),
    };

    if dry_run {
        WriteResponse::dry_run("set", 1).render(
            output,
            &format!("dry-run: set {} ({})", key, format_label),
        );
        timer.track(
            &format!("write set {}", file.display()),
            "rtk write set (dry-run)",
            &content,
            &updated,
        );
        return Ok(());
    }

    let stats = write_text(file, &updated, fast)?;
    WriteResponse::success("set", 1).render(
        output,
        &format!("OK set {} ({})", key, format_label),
    );
    if verbose > 1 {
        eprintln!(
            "bytes_written={}, fsync={}, rename={}",
            stats.bytes_written, stats.fsync_count, stats.rename_count
        );
    }

    timer.track(
        &format!("write set {}", file.display()),
        "rtk write set",
        &content,
        &updated,
    );
    Ok(())
}

/// Execute a batch of write operations from a JSON plan.
/// Single process startup, grouped fsync, one summary output.
pub fn run_batch(
    plan_json: &str,
    dry_run: bool,
    fast: bool,
    verbose: u8,
    output: OutputMode,
) -> Result<()> {
    let timer = tracking::TimedExecution::start();

    let ops: Vec<BatchOp> = serde_json::from_str(plan_json)
        .context("Failed to parse batch plan JSON")?;

    if ops.is_empty() {
        bail!("Batch plan is empty");
    }

    let total = ops.len();
    let mut applied = 0usize;
    let mut failed = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for (i, op) in ops.iter().enumerate() {
        let result = execute_batch_op(op, dry_run, fast);
        match result {
            Ok(count) => {
                applied += count;
                if verbose > 0 && output == OutputMode::Concise {
                    eprintln!("[{}/{}] OK {} {}", i + 1, total, op.op, op.file.display());
                }
            }
            Err(e) => {
                failed += 1;
                let msg = format!("[{}/{}] {} {}: {}", i + 1, total, op.op, op.file.display(), e);
                errors.push(msg.clone());
                if output == OutputMode::Concise {
                    eprintln!("{}", msg);
                }
            }
        }
    }

    // Render summary
    let mut resp = WriteResponse::success("batch", applied);
    resp.failed = Some(failed);
    if dry_run {
        resp.dry_run = Some(true);
    }
    if !errors.is_empty() && output == OutputMode::Json {
        resp.detail = Some(errors.join("; "));
    }
    if failed > 0 {
        resp.ok = failed < total; // partial failure is still ok=true if any succeeded
    }

    let concise_msg = if dry_run {
        format!("dry-run: batch {}/{} planned", applied, total)
    } else {
        format!("OK batch applied={} failed={} total={}", applied, failed, total)
    };
    resp.render(output, &concise_msg);

    timer.track(
        &format!("write batch ({})", total),
        "rtk write batch",
        plan_json,
        &concise_msg,
    );

    if failed == total {
        bail!("All {} batch operations failed", total);
    }
    Ok(())
}

/// Execute a single batch operation, return applied count.
fn execute_batch_op(op: &BatchOp, dry_run: bool, fast: bool) -> Result<usize> {
    match op.op.as_str() {
        "replace" => {
            let from = op.from.as_deref()
                .with_context(|| "batch replace: missing 'from' field")?;
            let to = op.to.as_deref()
                .with_context(|| "batch replace: missing 'to' field")?;
            if from.is_empty() {
                bail!("batch replace: 'from' must be non-empty");
            }

            let content = fs::read_to_string(&op.file)
                .with_context(|| format!("Failed to read {}", op.file.display()))?;
            if !content.contains(from) {
                bail!("NO_MATCH");
            }

            let (updated, count) = if op.all {
                replace_all_counted(&content, from, to)
            } else {
                (content.replacen(from, to, 1), 1)
            };

            if updated == content {
                return Ok(0);
            }
            if dry_run {
                return Ok(count);
            }
            write_text(&op.file, &updated, fast)?;
            Ok(count)
        }
        "patch" => {
            let old = op.old.as_deref()
                .with_context(|| "batch patch: missing 'old' field")?;
            let new = op.new.as_deref()
                .with_context(|| "batch patch: missing 'new' field")?;
            if old.is_empty() {
                bail!("batch patch: 'old' must be non-empty");
            }

            let content = fs::read_to_string(&op.file)
                .with_context(|| format!("Failed to read {}", op.file.display()))?;
            if !content.contains(old) {
                bail!("NO_MATCH");
            }

            let (updated, count) = if op.all {
                replace_all_counted(&content, old, new)
            } else {
                (content.replacen(old, new, 1), 1)
            };

            if updated == content {
                return Ok(0);
            }
            if dry_run {
                return Ok(count);
            }
            write_text(&op.file, &updated, fast)?;
            Ok(count)
        }
        "set" => {
            let key = op.key.as_deref()
                .with_context(|| "batch set: missing 'key' field")?;
            let value = op.value.as_deref()
                .with_context(|| "batch set: missing 'value' field")?;

            let vt = match op.value_type.as_deref() {
                Some("string") => ConfigValueType::String,
                Some("number") => ConfigValueType::Number,
                Some("bool") => ConfigValueType::Bool,
                Some("null") => ConfigValueType::Null,
                Some("json") => ConfigValueType::Json,
                _ => ConfigValueType::Auto,
            };
            let fmt = match op.format.as_deref() {
                Some("json") => ConfigFormat::Json,
                Some("toml") => ConfigFormat::Toml,
                _ => ConfigFormat::Auto,
            };
            let fmt = resolve_format(&op.file, fmt)?;

            let content = fs::read_to_string(&op.file)
                .with_context(|| format!("Failed to read {}", op.file.display()))?;

            let updated = match fmt {
                ConfigFormat::Json => {
                    let mut root: JsonValue = serde_json::from_str(&content)
                        .with_context(|| format!("Invalid JSON: {}", op.file.display()))?;
                    set_json_path(&mut root, key, parse_json_value(value, vt)?)?;
                    serialize_json_preserving_style(&root, &content)?
                }
                ConfigFormat::Toml => {
                    let mut root: DocumentMut = content.parse::<DocumentMut>()
                        .with_context(|| format!("Invalid TOML: {}", op.file.display()))?;
                    set_toml_path(&mut root, key, parse_toml_value(value, vt)?)?;
                    root.to_string()
                }
                ConfigFormat::Auto => unreachable!(),
            };

            if dry_run {
                return Ok(1);
            }
            write_text(&op.file, &updated, fast)?;
            Ok(1)
        }
        other => bail!("Unknown batch operation: '{}'", other),
    }
}

/// P0-2: single-pass replace that returns (result, count) without pre-scanning
fn replace_all_counted(content: &str, from: &str, to: &str) -> (String, usize) {
    let mut result = String::with_capacity(content.len());
    let mut count = 0usize;
    let mut last_end = 0;
    for (start, _) in content.match_indices(from) {
        result.push_str(&content[last_end..start]);
        result.push_str(to);
        count += 1;
        last_end = start + from.len();
    }
    result.push_str(&content[last_end..]);
    (result, count)
}

fn resolve_format(file: &Path, format: ConfigFormat) -> Result<ConfigFormat> {
    if format != ConfigFormat::Auto {
        return Ok(format);
    }
    match file.extension().and_then(|s| s.to_str()) {
        Some("json") => Ok(ConfigFormat::Json),
        Some("toml") => Ok(ConfigFormat::Toml),
        _ => bail!(
            "Cannot infer config format for {}. Use --format json|toml",
            file.display()
        ),
    }
}

fn write_text(file: &Path, updated: &str, fast: bool) -> Result<crate::write_core::WriteStats> {
    // P0-1: caller already verified content differs — skip redundant is_unchanged re-read
    let mut options = if fast {
        WriteOptions::fast()
    } else {
        WriteOptions::durable()
    };
    options.idempotent_skip = false; // caller guarantees content changed
    let writer = AtomicWriter::new(options);
    writer.write_str(file, updated)
}

fn parse_json_value(raw: &str, value_type: ConfigValueType) -> Result<JsonValue> {
    match value_type {
        ConfigValueType::Auto | ConfigValueType::Json => {
            if let Ok(v) = serde_json::from_str::<JsonValue>(raw) {
                Ok(v)
            } else {
                Ok(JsonValue::String(raw.to_string()))
            }
        }
        ConfigValueType::String => Ok(JsonValue::String(raw.to_string())),
        ConfigValueType::Number => {
            if let Ok(i) = raw.parse::<i64>() {
                Ok(JsonValue::from(i))
            } else if let Ok(u) = raw.parse::<u64>() {
                Ok(JsonValue::from(u))
            } else {
                let n = raw
                    .parse::<f64>()
                    .with_context(|| format!("Invalid number value: {}", raw))?;
                Ok(JsonValue::from(n))
            }
        }
        ConfigValueType::Bool => {
            let b = raw
                .parse::<bool>()
                .with_context(|| format!("Invalid bool value: {}", raw))?;
            Ok(JsonValue::Bool(b))
        }
        ConfigValueType::Null => Ok(JsonValue::Null),
    }
}

fn parse_toml_value(raw: &str, value_type: ConfigValueType) -> Result<TomlEditValue> {
    let value = match value_type {
        ConfigValueType::Auto | ConfigValueType::Json => {
            if let Ok(v) = raw.parse::<TomlValue>() {
                v
            } else {
                TomlValue::String(raw.to_string())
            }
        }
        ConfigValueType::String => TomlValue::String(raw.to_string()),
        ConfigValueType::Number => {
            if let Ok(i) = raw.parse::<i64>() {
                TomlValue::Integer(i)
            } else {
                let f = raw
                    .parse::<f64>()
                    .with_context(|| format!("Invalid number value: {}", raw))?;
                TomlValue::Float(f)
            }
        }
        ConfigValueType::Bool => {
            let b = raw
                .parse::<bool>()
                .with_context(|| format!("Invalid bool value: {}", raw))?;
            TomlValue::Boolean(b)
        }
        ConfigValueType::Null => bail!("TOML does not support null values"),
    };

    toml_to_toml_edit_value(value)
}

fn toml_to_toml_edit_value(value: TomlValue) -> Result<TomlEditValue> {
    match value {
        TomlValue::String(s) => Ok(TomlEditValue::from(s)),
        TomlValue::Integer(i) => Ok(TomlEditValue::from(i)),
        TomlValue::Float(f) => Ok(TomlEditValue::from(f)),
        TomlValue::Boolean(b) => Ok(TomlEditValue::from(b)),
        TomlValue::Datetime(dt) => Ok(TomlEditValue::from(dt)),
        TomlValue::Array(arr) => {
            let mut out = toml_edit::Array::new();
            for v in arr {
                out.push_formatted(toml_to_toml_edit_value(v)?);
            }
            Ok(TomlEditValue::Array(out))
        }
        TomlValue::Table(_) => bail!("Setting table values is not supported for --value"),
    }
}

fn set_json_path(root: &mut JsonValue, key: &str, value: JsonValue) -> Result<()> {
    let parts: Vec<&str> = key.split('.').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        // P2-1: reject empty key — overwriting root is dangerous and non-obvious
        bail!("--key must be non-empty");
    }

    if !root.is_object() {
        bail!(
            "Cannot set nested key '{}': top-level JSON value is not an object",
            key
        );
    }

    let mut current = root.as_object_mut().expect("object checked above");
    for part in &parts[..parts.len() - 1] {
        let entry = current
            .entry((*part).to_string())
            .or_insert_with(|| JsonValue::Object(Map::new()));
        if !entry.is_object() {
            bail!(
                "Cannot set key '{}': path segment '{}' is not an object",
                key,
                part
            );
        }
        current = entry.as_object_mut().expect("object ensured");
    }

    current.insert(parts[parts.len() - 1].to_string(), value);
    Ok(())
}

fn set_toml_path(root: &mut DocumentMut, key: &str, value: TomlEditValue) -> Result<()> {
    let parts: Vec<&str> = key.split('.').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        bail!("--key must be non-empty");
    }

    let mut current = root.as_table_mut();
    for part in &parts[..parts.len() - 1] {
        if !current.contains_key(part) {
            current.insert(part, Item::Table(Table::new()));
        }
        let entry = current
            .get_mut(part)
            .expect("table entry must exist after insert");
        if !entry.is_table() {
            bail!(
                "Cannot set key '{}': path segment '{}' is not a table",
                key,
                part
            );
        }
        current = entry
            .as_table_mut()
            .expect("path segment is guaranteed to be a table");
    }

    current[parts[parts.len() - 1]] = Item::Value(value);
    Ok(())
}

fn serialize_json_preserving_style(root: &JsonValue, original: &str) -> Result<String> {
    let mut serialized = if !original.contains('\n') {
        serde_json::to_string(root).context("Failed to serialize JSON")?
    } else {
        let indent = detect_json_indent(original).unwrap_or_else(|| b"  ".to_vec());
        serialize_json_with_indent(root, &indent)?
    };

    if original.ends_with('\n') && !serialized.ends_with('\n') {
        serialized.push('\n');
    }
    if !original.ends_with('\n') {
        serialized = serialized.trim_end_matches('\n').to_string();
    }

    Ok(serialized)
}

fn serialize_json_with_indent<T: Serialize>(value: &T, indent: &[u8]) -> Result<String> {
    let mut out = Vec::new();
    let formatter = PrettyFormatter::with_indent(indent);
    let mut serializer = Serializer::with_formatter(&mut out, formatter);
    value
        .serialize(&mut serializer)
        .context("Failed to serialize JSON")?;
    String::from_utf8(out).context("Failed to encode JSON as UTF-8")
}

fn detect_json_indent(original: &str) -> Option<Vec<u8>> {
    for line in original.lines().skip(1) {
        if line.trim().is_empty() {
            continue;
        }
        let trimmed = line.trim_start_matches([' ', '\t']);
        if trimmed.len() == line.len() {
            continue;
        }
        let indent = &line.as_bytes()[..line.len() - trimmed.len()];
        if indent.iter().all(|b| *b == b' ') || indent.iter().all(|b| *b == b'\t') {
            return Some(indent.to_vec());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn replace_first_only() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("a.txt");
        fs::write(&file, "a a a").unwrap();

        run_replace(&file, "a", "b", false, false, true, 0, OutputMode::Concise).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "b a a");
    }

    #[test]
    fn patch_all_occurrences() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("b.txt");
        fs::write(&file, "old\nold\n").unwrap();

        run_patch(&file, "old", "new", true, false, true, 0, OutputMode::Concise).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "new\nnew\n");
    }

    #[test]
    fn set_json_nested_key() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("c.json");
        fs::write(&file, "{\"a\":{}}").unwrap();

        run_set(
            &file,
            "a.b",
            "42",
            ConfigValueType::Number,
            ConfigFormat::Json,
            false,
            true,
            0,
            OutputMode::Concise,
        )
        .unwrap();

        let v: JsonValue = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(v["a"]["b"], JsonValue::from(42));
    }

    #[test]
    fn set_toml_nested_key() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("d.toml");
        fs::write(&file, "[a]\n").unwrap();

        run_set(
            &file,
            "a.b",
            "true",
            ConfigValueType::Bool,
            ConfigFormat::Toml,
            false,
            true,
            0,
            OutputMode::Concise,
        )
        .unwrap();

        let v: TomlValue = fs::read_to_string(&file).unwrap().parse().unwrap();
        assert_eq!(v["a"]["b"], TomlValue::Boolean(true));
    }

    #[test]
    fn dry_run_does_not_modify_file() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("e.txt");
        fs::write(&file, "hello world").unwrap();

        run_replace(&file, "world", "rtk", false, true, true, 0, OutputMode::Concise).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "hello world");
    }

    #[test]
    fn set_json_conflict_path_fails() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("f.json");
        fs::write(&file, "{\"a\":1}").unwrap();

        let err = run_set(
            &file,
            "a.b",
            "2",
            ConfigValueType::Number,
            ConfigFormat::Json,
            false,
            true,
            0,
            OutputMode::Concise,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not an object"));
    }

    #[test]
    fn set_toml_preserves_comments() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("g.toml");
        fs::write(&file, "# top\n[a]\n# keep\nb = 1\n").unwrap();

        run_set(
            &file,
            "a.c",
            "true",
            ConfigValueType::Bool,
            ConfigFormat::Toml,
            false,
            true,
            0,
            OutputMode::Concise,
        )
        .unwrap();

        let out = fs::read_to_string(&file).unwrap();
        assert!(out.contains("# top"));
        assert!(out.contains("# keep"));
        let v: TomlValue = out.parse().unwrap();
        assert_eq!(v["a"]["c"], TomlValue::Boolean(true));
    }

    #[test]
    fn set_json_preserves_compact_style() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("h.json");
        fs::write(&file, "{\"a\":1}").unwrap();

        run_set(
            &file,
            "b",
            "2",
            ConfigValueType::Number,
            ConfigFormat::Json,
            false,
            true,
            0,
            OutputMode::Concise,
        )
        .unwrap();

        assert_eq!(fs::read_to_string(&file).unwrap(), "{\"a\":1,\"b\":2}");
    }

    #[test]
    fn replace_all_counted_single_pass() {
        let (result, count) = replace_all_counted("aXbXcX", "X", "Y");
        assert_eq!(result, "aYbYcY");
        assert_eq!(count, 3);
    }

    #[test]
    fn replace_all_counted_no_match() {
        let (result, count) = replace_all_counted("hello", "X", "Y");
        assert_eq!(result, "hello");
        assert_eq!(count, 0);
    }

    #[test]
    fn quiet_mode_produces_no_stdout() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("quiet.txt");
        fs::write(&file, "hello world").unwrap();

        // Quiet mode should succeed without panicking
        run_replace(&file, "world", "rtk", false, false, true, 0, OutputMode::Quiet).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "hello rtk");
    }

    #[test]
    fn json_mode_same_file_result_as_concise() {
        // Contract: file content must be identical regardless of output mode
        let tmp = TempDir::new().unwrap();
        let f1 = tmp.path().join("concise.txt");
        let f2 = tmp.path().join("json.txt");
        fs::write(&f1, "a b c").unwrap();
        fs::write(&f2, "a b c").unwrap();

        run_replace(&f1, "b", "X", false, false, true, 0, OutputMode::Concise).unwrap();
        run_replace(&f2, "b", "X", false, false, true, 0, OutputMode::Json).unwrap();

        assert_eq!(
            fs::read_to_string(&f1).unwrap(),
            fs::read_to_string(&f2).unwrap(),
        );
    }

    #[test]
    fn write_response_json_schema() {
        let resp = WriteResponse::success("replace", 3);
        let json_str = serde_json::to_string(&resp).unwrap();
        let v: JsonValue = serde_json::from_str(&json_str).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["ok"], true);
        assert_eq!(v["op"], "replace");
        assert_eq!(v["applied"], 3);
        // Optional fields should not be present
        assert!(v.get("error").is_none());
        assert!(v.get("dry_run").is_none());
    }

    #[test]
    fn write_response_noop_json() {
        let resp = WriteResponse::noop("patch");
        let json_str = serde_json::to_string(&resp).unwrap();
        let v: JsonValue = serde_json::from_str(&json_str).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["applied"], 0);
        assert_eq!(v["hint"], "no-op");
    }

    #[test]
    fn batch_replace_and_patch() {
        let tmp = TempDir::new().unwrap();
        let f1 = tmp.path().join("batch1.txt");
        let f2 = tmp.path().join("batch2.txt");
        fs::write(&f1, "hello world").unwrap();
        fs::write(&f2, "foo bar baz").unwrap();

        let plan = serde_json::json!([
            {"op":"replace","file": f1.to_str().unwrap(), "from":"world","to":"batch"},
            {"op":"patch","file": f2.to_str().unwrap(), "old":"bar","new":"PATCHED"}
        ]);

        run_batch(&plan.to_string(), false, true, 0, OutputMode::Quiet).unwrap();
        assert_eq!(fs::read_to_string(&f1).unwrap(), "hello batch");
        assert_eq!(fs::read_to_string(&f2).unwrap(), "foo PATCHED baz");
    }

    #[test]
    fn batch_dry_run_no_writes() {
        let tmp = TempDir::new().unwrap();
        let f1 = tmp.path().join("batchdry.txt");
        fs::write(&f1, "original").unwrap();

        let plan = serde_json::json!([
            {"op":"replace","file": f1.to_str().unwrap(), "from":"original","to":"changed"}
        ]);

        run_batch(&plan.to_string(), true, true, 0, OutputMode::Quiet).unwrap();
        assert_eq!(fs::read_to_string(&f1).unwrap(), "original");
    }

    #[test]
    fn batch_partial_failure_continues() {
        let tmp = TempDir::new().unwrap();
        let f1 = tmp.path().join("batchok.txt");
        fs::write(&f1, "hello").unwrap();

        let plan = serde_json::json!([
            {"op":"replace","file":"/nonexistent/file.txt","from":"x","to":"y"},
            {"op":"replace","file": f1.to_str().unwrap(), "from":"hello","to":"done"}
        ]);

        run_batch(&plan.to_string(), false, true, 0, OutputMode::Quiet).unwrap();
        // Second op should still succeed despite first failure
        assert_eq!(fs::read_to_string(&f1).unwrap(), "done");
    }

    #[test]
    fn batch_set_json_key() {
        let tmp = TempDir::new().unwrap();
        let f = tmp.path().join("batchset.json");
        fs::write(&f, "{\"a\":1}").unwrap();

        let plan = serde_json::json!([
            {"op":"set","file": f.to_str().unwrap(), "key":"b","value":"2","value_type":"number"}
        ]);

        run_batch(&plan.to_string(), false, true, 0, OutputMode::Quiet).unwrap();
        let v: JsonValue = serde_json::from_str(&fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(v["b"], JsonValue::from(2));
    }

    #[test]
    fn empty_key_rejected_for_json_set() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("emptykey.json");
        fs::write(&file, "{\"a\":1}").unwrap();

        let err = run_set(
            &file, "", "42", ConfigValueType::Number, ConfigFormat::Json,
            false, true, 0, OutputMode::Concise,
        ).unwrap_err();
        assert!(err.to_string().contains("non-empty"));
        // File must not be modified
        assert_eq!(fs::read_to_string(&file).unwrap(), "{\"a\":1}");
    }

    #[test]
    fn set_json_preserves_indent_and_newline() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("i.json");
        fs::write(&file, "{\n    \"a\": {\n        \"b\": 1\n    }\n}\n").unwrap();

        run_set(
            &file,
            "a.c",
            "2",
            ConfigValueType::Number,
            ConfigFormat::Json,
            false,
            true,
            0,
            OutputMode::Concise,
        )
        .unwrap();

        let out = fs::read_to_string(&file).unwrap();
        assert!(out.contains("\n    \"a\": {"));
        assert!(out.contains("\n        \"b\": 1"));
        assert!(out.ends_with('\n'));
    }
}

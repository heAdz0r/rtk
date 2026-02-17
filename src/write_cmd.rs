use crate::tracking;
use crate::write_core::{AtomicWriter, WriteOptions};
use crate::write_semantics::{semantics_for, WriteOperation};
use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use serde::Serialize;
use serde_json::ser::{PrettyFormatter, Serializer};
use serde_json::{Map, Value as JsonValue};
use std::fs;
use std::path::Path;
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

pub fn run_replace(
    file: &Path,
    from: &str,
    to: &str,
    all: bool,
    dry_run: bool,
    fast: bool,
    verbose: u8,
) -> Result<()> {
    let timer = tracking::TimedExecution::start();
    let semantics = semantics_for(WriteOperation::Replace);

    if from.is_empty() {
        bail!("--from must be non-empty");
    }

    let content =
        fs::read_to_string(file).with_context(|| format!("Failed to read {}", file.display()))?;
    let count = content.matches(from).count();
    if count == 0 {
        bail!("No matches found for --from in {}", file.display());
    }

    let updated = if all {
        content.replace(from, to)
    } else {
        content.replacen(from, to, 1)
    };

    if updated == content {
        let msg = format!(
            "no-op: replacement does not change {} ({:?}/{:?})",
            file.display(),
            semantics.operation,
            semantics.class
        );
        println!("{}", msg);
        timer.track(
            &format!("write replace {}", file.display()),
            "rtk write replace (noop)",
            &content,
            &updated,
        );
        return Ok(());
    }

    if dry_run {
        let planned = if all { count } else { 1 };
        println!(
            "dry-run: replace {} occurrence(s) in {} ({:?}/{:?})",
            planned,
            file.display(),
            semantics.operation,
            semantics.class
        );
        timer.track(
            &format!("write replace {}", file.display()),
            "rtk write replace (dry-run)",
            &content,
            &updated,
        );
        return Ok(());
    }

    let stats = write_text(file, &updated, fast)?;
    let applied = if all { count } else { 1 };
    println!(
        "ok replace: {} occurrence(s), skipped={}, fsync={}, rename={}",
        applied, stats.skipped_unchanged, stats.fsync_count, stats.rename_count
    );
    if verbose > 1 {
        eprintln!("bytes_written={}", stats.bytes_written);
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
) -> Result<()> {
    let timer = tracking::TimedExecution::start();
    let semantics = semantics_for(WriteOperation::Patch);

    if old.is_empty() {
        bail!("--old must be non-empty");
    }

    let content =
        fs::read_to_string(file).with_context(|| format!("Failed to read {}", file.display()))?;
    let count = content.matches(old).count();
    if count == 0 {
        bail!("Patch hunk not found in {}", file.display());
    }

    let updated = if all {
        content.replace(old, new)
    } else {
        content.replacen(old, new, 1)
    };

    if updated == content {
        let msg = format!(
            "no-op: patch does not change {} ({:?}/{:?})",
            file.display(),
            semantics.operation,
            semantics.class
        );
        println!("{}", msg);
        timer.track(
            &format!("write patch {}", file.display()),
            "rtk write patch (noop)",
            &content,
            &updated,
        );
        return Ok(());
    }

    if dry_run {
        let planned = if all { count } else { 1 };
        println!(
            "dry-run: patch {} hunk(s) in {} ({:?}/{:?})",
            planned,
            file.display(),
            semantics.operation,
            semantics.class
        );
        timer.track(
            &format!("write patch {}", file.display()),
            "rtk write patch (dry-run)",
            &content,
            &updated,
        );
        return Ok(());
    }

    let stats = write_text(file, &updated, fast)?;
    let applied = if all { count } else { 1 };
    println!(
        "ok patch: {} hunk(s), skipped={}, fsync={}, rename={}",
        applied, stats.skipped_unchanged, stats.fsync_count, stats.rename_count
    );
    if verbose > 1 {
        eprintln!("bytes_written={}", stats.bytes_written);
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
) -> Result<()> {
    let timer = tracking::TimedExecution::start();
    let semantics = semantics_for(WriteOperation::Set);
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
        println!(
            "dry-run: set {} in {} ({}, {:?}/{:?})",
            key,
            file.display(),
            format_label,
            semantics.operation,
            semantics.class
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
    println!(
        "ok set: {} ({}) skipped={}, fsync={}, rename={}",
        key, format_label, stats.skipped_unchanged, stats.fsync_count, stats.rename_count
    );
    if verbose > 1 {
        eprintln!("bytes_written={}", stats.bytes_written);
    }

    timer.track(
        &format!("write set {}", file.display()),
        "rtk write set",
        &content,
        &updated,
    );
    Ok(())
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
    let options = if fast {
        WriteOptions::fast()
    } else {
        WriteOptions::durable()
    };
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
        *root = value;
        return Ok(());
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
        if !current.contains_key(*part) {
            current.insert(*part, Item::Table(Table::new()));
        }
        let entry = current
            .get_mut(*part)
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

        run_replace(&file, "a", "b", false, false, true, 0).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "b a a");
    }

    #[test]
    fn patch_all_occurrences() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("b.txt");
        fs::write(&file, "old\nold\n").unwrap();

        run_patch(&file, "old", "new", true, false, true, 0).unwrap();
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

        run_replace(&file, "world", "rtk", false, true, true, 0).unwrap();
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
        )
        .unwrap();

        assert_eq!(fs::read_to_string(&file).unwrap(), "{\"a\":1,\"b\":2}");
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
        )
        .unwrap();

        let out = fs::read_to_string(&file).unwrap();
        assert!(out.contains("\n    \"a\": {"));
        assert!(out.contains("\n        \"b\": 1"));
        assert!(out.ends_with('\n'));
    }
}

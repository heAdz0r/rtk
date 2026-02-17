use anyhow::{bail, Context, Result};
use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File, Metadata};
use std::hash::Hasher;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};
use tempfile::NamedTempFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurabilityMode {
    Durable,
    Fast,
}

// P1-5: CAS infrastructure — will be used by CAS CLI flags (PR-W6 P0-1)
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct FileSnapshot {
    pub len: Option<u64>,
    pub modified: Option<SystemTime>,
    pub hash: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct CasOptions {
    pub expected_len: Option<u64>,
    pub expected_modified: Option<SystemTime>,
    pub expected_hash: Option<u64>,
}

impl CasOptions {
    #[allow(dead_code)] // P1-5: will be used by CAS CLI flags
    pub fn from_snapshot(snapshot: &FileSnapshot) -> Self {
        Self {
            expected_len: snapshot.len,
            expected_modified: snapshot.modified,
            expected_hash: snapshot.hash,
        }
    }

    fn has_expectations(&self) -> bool {
        self.expected_len.is_some()
            || self.expected_modified.is_some()
            || self.expected_hash.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct WriteOptions {
    pub durability: DurabilityMode,
    pub buffer_size: usize,
    pub preserve_permissions: bool,
    pub idempotent_skip: bool,
    pub compare_hash_when_same_size: bool,
    pub cas: Option<CasOptions>,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            durability: DurabilityMode::Durable,
            buffer_size: 64 * 1024,
            preserve_permissions: true,
            idempotent_skip: true,
            compare_hash_when_same_size: false,
            cas: None,
        }
    }
}

impl WriteOptions {
    pub fn durable() -> Self {
        Self::default()
    }

    pub fn fast() -> Self {
        Self {
            durability: DurabilityMode::Fast,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct WriteStats {
    pub bytes_written: u64,
    pub fsync_count: u32,
    pub rename_count: u32,
    pub elapsed: Duration,
    pub skipped_unchanged: bool,
    pub durability: DurabilityMode,
}

impl WriteStats {
    fn skipped(durability: DurabilityMode, start: Instant) -> Self {
        Self {
            bytes_written: 0,
            fsync_count: 0,
            rename_count: 0,
            elapsed: start.elapsed(),
            skipped_unchanged: true,
            durability,
        }
    }
}

pub struct AtomicWriter {
    options: WriteOptions,
}

impl AtomicWriter {
    pub fn new(options: WriteOptions) -> Self {
        Self { options }
    }

    pub fn write_str(&self, path: &Path, content: &str) -> Result<WriteStats> {
        self.write_bytes(path, content.as_bytes())
    }

    pub fn write_bytes(&self, path: &Path, content: &[u8]) -> Result<WriteStats> {
        let start = Instant::now();
        let parent = path.parent().with_context(|| {
            format!(
                "Cannot write to {}: path has no parent directory",
                path.display()
            )
        })?;

        let existing_meta = if path.exists() {
            Some(fs::metadata(path).with_context(|| format!("Failed to stat {}", path.display()))?)
        } else {
            None
        };

        if let Some(cas) = &self.options.cas {
            verify_cas(path, existing_meta.as_ref(), cas)?;
        }

        if self.options.idempotent_skip {
            if let Some(meta) = existing_meta.as_ref() {
                if is_unchanged(
                    path,
                    meta,
                    content,
                    self.options.compare_hash_when_same_size,
                )? {
                    return Ok(WriteStats::skipped(self.options.durability, start));
                }
            }
        }

        let mut fsync_count = 0u32;

        let mut temp_file = NamedTempFile::new_in(parent)
            .with_context(|| format!("Failed to create temp file in {}", parent.display()))?;

        {
            let mut writer =
                BufWriter::with_capacity(self.options.buffer_size.max(1), temp_file.as_file_mut());
            writer
                .write_all(content)
                .with_context(|| format!("Failed to write {} bytes to temp file", content.len()))?;
            writer.flush().context("Failed to flush temp file")?;
        }

        if self.options.preserve_permissions {
            if let Some(meta) = existing_meta.as_ref() {
                fs::set_permissions(temp_file.path(), meta.permissions()).with_context(|| {
                    format!(
                        "Failed to preserve permissions while writing {}",
                        path.display()
                    )
                })?;
            }
        }

        if self.options.durability == DurabilityMode::Durable {
            temp_file
                .as_file()
                .sync_data()
                .with_context(|| format!("Failed to sync temp data for {}", path.display()))?;
            fsync_count += 1;
        }

        // P1-4: preserve error chain via .context() instead of formatting into anyhow!
        temp_file.persist(path).map_err(|e| {
            anyhow::Error::new(e.error)
                .context(format!("Failed to atomically replace {}", path.display()))
        })?;

        if self.options.durability == DurabilityMode::Durable {
            fsync_parent_dir(parent)
                .with_context(|| format!("Failed to sync parent dir {}", parent.display()))?;
            fsync_count += 1;
        }

        Ok(WriteStats {
            bytes_written: content.len() as u64,
            fsync_count,
            rename_count: 1,
            elapsed: start.elapsed(),
            skipped_unchanged: false,
            durability: self.options.durability,
        })
    }
}

#[allow(dead_code)] // P1-5: will be used by CAS CLI flags
pub fn snapshot_file(path: &Path, include_hash: bool) -> Result<Option<FileSnapshot>> {
    if !path.exists() {
        return Ok(None);
    }

    let metadata =
        fs::metadata(path).with_context(|| format!("Failed to stat {}", path.display()))?;
    let modified = metadata.modified().ok();
    let hash = if include_hash {
        Some(hash_file(path)?)
    } else {
        None
    };

    Ok(Some(FileSnapshot {
        len: Some(metadata.len()),
        modified,
        hash,
    }))
}

fn verify_cas(path: &Path, metadata: Option<&Metadata>, cas: &CasOptions) -> Result<()> {
    if metadata.is_none() {
        if cas.has_expectations() {
            bail!("CAS mismatch for {}: target does not exist", path.display());
        }
        return Ok(());
    }

    let metadata = metadata.expect("checked above");

    if let Some(expected_len) = cas.expected_len {
        if metadata.len() != expected_len {
            bail!(
                "CAS mismatch for {}: expected len {}, got {}",
                path.display(),
                expected_len,
                metadata.len()
            );
        }
    }

    if let Some(expected_modified) = cas.expected_modified {
        let actual_modified = metadata.modified().with_context(|| {
            format!(
                "Failed to read modification time for {} during CAS",
                path.display()
            )
        })?;
        if actual_modified != expected_modified {
            bail!(
                "CAS mismatch for {}: modification time changed",
                path.display()
            );
        }
    }

    if let Some(expected_hash) = cas.expected_hash {
        let actual_hash = hash_file(path)?;
        if actual_hash != expected_hash {
            bail!("CAS mismatch for {}: content hash changed", path.display());
        }
    }

    Ok(())
}

fn is_unchanged(
    path: &Path,
    metadata: &Metadata,
    content: &[u8],
    compare_hash: bool,
) -> Result<bool> {
    if metadata.len() != content.len() as u64 {
        return Ok(false);
    }

    if compare_hash {
        let existing_hash = hash_file(path)?;
        let content_hash = hash_bytes(content);
        if existing_hash != content_hash {
            return Ok(false);
        }
    }

    file_equals_bytes(path, content)
}

fn file_equals_bytes(path: &Path, expected: &[u8]) -> Result<bool> {
    let mut file = File::open(path)
        .with_context(|| format!("Failed to read existing file {}", path.display()))?;
    let mut buf = [0u8; 8192];
    let mut offset = 0usize;

    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("Failed to read existing file {}", path.display()))?;
        if n == 0 {
            return Ok(offset == expected.len());
        }
        if offset + n > expected.len() {
            return Ok(false);
        }
        if expected.get(offset..offset + n) != Some(&buf[..n]) {
            return Ok(false);
        }
        offset += n;
    }
}

fn hash_bytes(content: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    hasher.write(content);
    hasher.finish()
}

fn hash_file(path: &Path) -> Result<u64> {
    let mut file = File::open(path)
        .with_context(|| format!("Failed to read existing file {}", path.display()))?;
    let mut hasher = DefaultHasher::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("Failed to read existing file {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.write(&buf[..n]);
    }
    Ok(hasher.finish())
}

#[cfg(unix)]
fn fsync_parent_dir(parent: &Path) -> Result<()> {
    let dir = File::open(parent)
        .with_context(|| format!("Failed to open parent dir {}", parent.display()))?;
    dir.sync_all()
        .with_context(|| format!("Failed to fsync parent dir {}", parent.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn fsync_parent_dir(_parent: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn writes_and_skips_unchanged_content() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a.txt");
        let writer = AtomicWriter::new(WriteOptions::default());

        let first = writer.write_str(&path, "hello").unwrap();
        assert!(!first.skipped_unchanged);
        assert_eq!(first.rename_count, 1);
        assert_eq!(first.bytes_written, 5);

        let second = writer.write_str(&path, "hello").unwrap();
        assert!(second.skipped_unchanged);
        assert_eq!(second.rename_count, 0);
        assert_eq!(second.bytes_written, 0);
    }

    #[test]
    fn fast_mode_avoids_fsync() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("b.txt");
        let writer = AtomicWriter::new(WriteOptions::fast());

        let stats = writer.write_str(&path, "hello").unwrap();
        assert_eq!(stats.durability, DurabilityMode::Fast);
        assert_eq!(stats.fsync_count, 0);
        assert_eq!(stats.rename_count, 1);
    }

    #[test]
    fn disable_idempotent_skip_always_writes() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("e.txt");
        fs::write(&path, "hello").unwrap();

        let mut opts = WriteOptions::default();
        opts.idempotent_skip = false;
        let writer = AtomicWriter::new(opts);
        let stats = writer.write_str(&path, "hello").unwrap();
        assert!(!stats.skipped_unchanged);
        assert_eq!(stats.rename_count, 1);
    }

    #[test]
    fn cas_mismatch_rejects_write() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("c.txt");
        fs::write(&path, "hello").unwrap();

        let mut opts = WriteOptions::default();
        opts.cas = Some(CasOptions {
            expected_len: Some(999),
            expected_modified: None,
            expected_hash: None,
        });
        let writer = AtomicWriter::new(opts);

        let err = writer.write_str(&path, "new content").unwrap_err();
        assert!(err.to_string().contains("CAS mismatch"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn cas_snapshot_allows_expected_write() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("d.txt");
        fs::write(&path, "hello").unwrap();

        let snapshot = snapshot_file(&path, true).unwrap().unwrap();
        let mut opts = WriteOptions::default();
        opts.cas = Some(CasOptions::from_snapshot(&snapshot));
        let writer = AtomicWriter::new(opts);

        let stats = writer.write_str(&path, "world").unwrap();
        assert!(!stats.skipped_unchanged);
        assert_eq!(fs::read_to_string(&path).unwrap(), "world");
    }
}

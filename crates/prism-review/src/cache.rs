// SPDX-License-Identifier: AGPL-3.0-only
//! Optional content-addressed artifact cache (`(input SHA-256, engine version, options hash) -> artifacts`)
//! with a pluggable [`ArtifactCache`] backend trait and [`FilesystemCache`] implementation with
//! size-based eviction.

use prism_core::ENGINE_VERSION;
use prism_core::error::{Error, Result};
use prism_core::hash::sha256_hex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Kind of cached artifact.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "artifact", rename_all = "snake_case")]
pub enum CacheArtifactKind {
    /// Rendered SVG page (`0`-based `page_index`).
    PageSvg {
        /// Zero-based page index.
        page_index: u32,
    },
    /// Rendered PNG page (`0`-based `page_index`, DPI in milli-dots-per-inch).
    PagePng {
        /// Zero-based page index.
        page_index: u32,
        /// Resolution in milli-DPI (e.g. `150_000` for `150.0` DPI).
        dpi_milli: u32,
    },
    /// Extracted UTF-8 plain text.
    ExtractedText,
    /// Serialized [`prism_core::review::HiddenContentReport`] JSON.
    HiddenContentReport,
    /// Serialized [`prism_core::tree::FamilyTree`] JSON.
    FamilyTree,
}

impl CacheArtifactKind {
    /// Deterministic slug used in cache keys and file names.
    #[must_use]
    pub fn slug(&self) -> String {
        match self {
            Self::PageSvg { page_index } => format!("page-svg-{page_index}"),
            Self::PagePng {
                page_index,
                dpi_milli,
            } => format!("page-png-{page_index}-{dpi_milli}"),
            Self::ExtractedText => "extracted-text".to_string(),
            Self::HiddenContentReport => "hidden-content-report".to_string(),
            Self::FamilyTree => "family-tree".to_string(),
        }
    }
}

/// Content-addressed cache key combining the input SHA-256, [`ENGINE_VERSION`], options SHA-256,
/// and [`CacheArtifactKind`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CacheKey {
    /// Lowercase hex SHA-256 of the source document bytes.
    pub input_sha256: String,
    /// Prism engine + layout algorithm version string.
    pub engine_version: String,
    /// Lowercase hex SHA-256 of canonicalized options JSON.
    pub options_sha256: String,
    /// Artifact kind.
    pub kind: CacheArtifactKind,
}

impl CacheKey {
    /// Construct a [`CacheKey`] from raw document bytes, serializable options, and `kind`.
    #[must_use]
    pub fn new(document_bytes: &[u8], options_json: &str, kind: CacheArtifactKind) -> Self {
        Self {
            input_sha256: sha256_hex(document_bytes),
            engine_version: ENGINE_VERSION.to_string(),
            options_sha256: sha256_hex(options_json.as_bytes()),
            kind,
        }
    }

    /// Compute the composite SHA-256 digest uniquely identifying this cache entry.
    #[must_use]
    pub fn digest_hex(&self) -> String {
        let canonical = format!(
            "{}|{}|{}|{}",
            self.input_sha256,
            self.engine_version,
            self.options_sha256,
            self.kind.slug()
        );
        sha256_hex(canonical.as_bytes())
    }
}

/// Pluggable storage backend trait for content-addressed artifact caching.
///
/// Implementations must guarantee that a cache hit returns the exact bytes stored by [`Self::put`].
pub trait ArtifactCache: Send + Sync {
    /// Look up cached artifact bytes for `key`.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the underlying storage backend fails.
    fn get(&self, key: &CacheKey) -> Result<Option<Vec<u8>>>;

    /// Store `bytes` under `key`, evicting older entries if needed.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the underlying storage backend fails.
    fn put(&self, key: &CacheKey, bytes: &[u8]) -> Result<()>;

    /// Remove a single entry from the cache.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if removal fails.
    fn invalidate(&self, key: &CacheKey) -> Result<()>;

    /// Remove all entries from the cache.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if clearing fails.
    fn clear(&self) -> Result<()>;
}

/// Filesystem-backed [`ArtifactCache`] with size-based eviction.
#[derive(Debug, Clone)]
pub struct FilesystemCache {
    root_dir: PathBuf,
    max_bytes: u64,
}

impl FilesystemCache {
    /// Create or open a [`FilesystemCache`] rooted at `root_dir` with a maximum total capacity of `max_bytes`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if `root_dir` cannot be created.
    pub fn new(root_dir: impl Into<PathBuf>, max_bytes: u64) -> Result<Self> {
        let root_dir = root_dir.into();
        fs::create_dir_all(&root_dir)?;
        Ok(Self {
            root_dir,
            max_bytes: max_bytes.max(64),
        })
    }

    /// Root directory of the cache.
    #[must_use]
    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    /// Maximum total byte capacity before eviction runs.
    #[must_use]
    pub const fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// Compute the current total byte size of all `.bin` artifact files in the cache.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if directory traversal fails.
    pub fn total_bytes(&self) -> Result<u64> {
        let files = self.list_cache_files()?;
        Ok(files.iter().map(|f| f.size).sum())
    }

    fn entry_path(&self, key: &CacheKey) -> PathBuf {
        let digest = key.digest_hex();
        let prefix = &digest[..2];
        self.root_dir.join(prefix).join(format!("{digest}.bin"))
    }

    fn evict_if_needed(&self, incoming_len: u64) -> Result<()> {
        let mut files = self.list_cache_files()?;
        let mut current_total: u64 = files.iter().map(|f| f.size).sum();

        if current_total.saturating_add(incoming_len) <= self.max_bytes {
            return Ok(());
        }

        // Sort oldest-modified first, breaking ties deterministically by path
        files.sort_by(|a, b| {
            a.modified
                .cmp(&b.modified)
                .then_with(|| a.path.cmp(&b.path))
        });

        for entry in files {
            if current_total.saturating_add(incoming_len) <= self.max_bytes {
                break;
            }
            if fs::remove_file(&entry.path).is_ok() {
                current_total = current_total.saturating_sub(entry.size);
            }
        }
        Ok(())
    }

    fn list_cache_files(&self) -> Result<Vec<CacheFileMeta>> {
        let mut out = Vec::new();
        if !self.root_dir.exists() {
            return Ok(out);
        }
        for shard_res in fs::read_dir(&self.root_dir)? {
            let shard = shard_res?;
            let shard_path = shard.path();
            if !shard_path.is_dir() {
                continue;
            }
            for entry_res in fs::read_dir(&shard_path)? {
                let entry = entry_res?;
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("bin") {
                    continue;
                }
                if let Ok(meta) = entry.metadata() {
                    out.push(CacheFileMeta {
                        path,
                        size: meta.len(),
                        modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    });
                }
            }
        }
        Ok(out)
    }
}

struct CacheFileMeta {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
}

impl ArtifactCache for FilesystemCache {
    fn get(&self, key: &CacheKey) -> Result<Option<Vec<u8>>> {
        let path = self.entry_path(key);
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read(&path)?;
        if raw.len() < 64 {
            let _ = fs::remove_file(&path);
            return Ok(None);
        }
        let stored_sha = std::str::from_utf8(&raw[..64]).unwrap_or("");
        let payload = &raw[64..];
        let actual_sha = sha256_hex(payload);
        if !stored_sha.eq_ignore_ascii_case(&actual_sha) {
            let _ = fs::remove_file(&path);
            return Ok(None);
        }
        Ok(Some(payload.to_vec()))
    }

    fn put(&self, key: &CacheKey, bytes: &[u8]) -> Result<()> {
        let framed_len = (bytes.len() + 64) as u64;
        if framed_len > self.max_bytes {
            // Do not cache single items larger than the entire cache budget
            return Ok(());
        }
        self.evict_if_needed(framed_len)?;

        let path = self.entry_path(key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let payload_sha = sha256_hex(bytes);
        let mut framed = Vec::with_capacity(64 + bytes.len());
        framed.extend_from_slice(payload_sha.as_bytes());
        framed.extend_from_slice(bytes);
        fs::write(&path, framed).map_err(Error::Io)
    }

    fn invalidate(&self, key: &CacheKey) -> Result<()> {
        let path = self.entry_path(key);
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    fn clear(&self) -> Result<()> {
        for f in self.list_cache_files()? {
            let _ = fs::remove_file(f.path);
        }
        Ok(())
    }
}

// SPDX-License-Identifier: AGPL-3.0-only
//! Random-access [`DocumentSource`] abstraction (`Bytes` and file-backed `Read + Seek`)
//! for inspecting and opening large documents and containers without copying full buffers.

use crate::error::{Error, Result};
use crate::format::{DetectionResult, detect_format};
use bytes::Bytes;
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Default prefix byte length (`16 KiB`) read for magic-byte format detection on file sources.
pub const DEFAULT_DETECTION_PREFIX_BYTES: usize = 16 * 1024;

/// Combined trait for random-access readers (`Read + Seek + Send`).
pub trait SeekableReader: Read + Seek + Send {}

impl<T: Read + Seek + Send> SeekableReader for T {}

/// A random-access document input source backed either by an in-memory [`Bytes`] buffer
/// or a filesystem path opened on demand via `Read + Seek`.
#[derive(Debug, Clone)]
pub enum DocumentSource {
    /// In-memory byte buffer.
    Bytes {
        /// Shared byte slice.
        data: Bytes,
        /// Optional filename hint.
        filename: Option<String>,
    },
    /// File on disk (inspected via `Read + Seek` without reading the whole file upfront).
    File {
        /// Filesystem path to the document.
        path: PathBuf,
        /// File size in bytes at the time the source was created.
        len: u64,
    },
}

impl DocumentSource {
    /// Create an in-memory [`DocumentSource::Bytes`] from a byte buffer and optional filename hint.
    #[must_use]
    pub fn from_bytes(data: impl Into<Bytes>, filename: Option<String>) -> Self {
        Self::Bytes {
            data: data.into(),
            filename,
        }
    }

    /// Create a file-backed [`DocumentSource::File`] from a filesystem path.
    ///
    /// Queries file metadata (`len`) without reading the file contents into memory.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the file does not exist or cannot be stat'd.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path_buf = path.as_ref().to_path_buf();
        let meta = std::fs::metadata(&path_buf)?;
        Ok(Self::File {
            path: path_buf,
            len: meta.len(),
        })
    }

    /// Total length of the document source in bytes.
    #[must_use]
    pub fn len(&self) -> u64 {
        match self {
            Self::Bytes { data, .. } => data.len() as u64,
            Self::File { len, .. } => *len,
        }
    }

    /// Returns `true` if the document source has length `0`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Return the filename hint or the file's basename.
    #[must_use]
    pub fn filename(&self) -> Option<&str> {
        match self {
            Self::Bytes { filename, .. } => filename.as_deref(),
            Self::File { path, .. } => path.file_name().and_then(|s| s.to_str()),
        }
    }

    /// Return the filesystem path if this is a [`DocumentSource::File`].
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Bytes { .. } => None,
            Self::File { path, .. } => Some(path.as_path()),
        }
    }

    /// Return the underlying slice if this source is already in memory (`DocumentSource::Bytes`).
    #[must_use]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes { data, .. } => Some(data.as_ref()),
            Self::File { .. } => None,
        }
    }

    /// Open a random-access [`SeekableReader`] (`Read + Seek + Send`) over the document source.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if opening the underlying file fails.
    pub fn open_reader(&self) -> Result<Box<dyn SeekableReader>> {
        match self {
            Self::Bytes { data, .. } => Ok(Box::new(Cursor::new(data.clone()))),
            Self::File { path, .. } => {
                let file = File::open(path)?;
                Ok(Box::new(BufReader::new(file)))
            }
        }
    }

    /// Read at most `max_bytes` from the start of the document in `O(min(len, max_bytes))` memory.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if reading from the file fails.
    pub fn read_prefix(&self, max_bytes: usize) -> Result<Vec<u8>> {
        match self {
            Self::Bytes { data, .. } => {
                let take = data.len().min(max_bytes);
                Ok(data[..take].to_vec())
            }
            Self::File { path, len } => {
                #[allow(clippy::cast_possible_truncation)]
                let cap = (*len).min(max_bytes as u64) as usize;
                let mut buf = Vec::with_capacity(cap);
                let file = File::open(path)?;
                file.take(cap as u64).read_to_end(&mut buf)?;
                Ok(buf)
            }
        }
    }

    /// Read a byte range `[offset .. offset + len]` via `Seek + Read` without loading the rest of the file.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] or [`Error::InvalidInput`] if the range is out of bounds.
    pub fn read_range(&self, offset: u64, length: usize) -> Result<Vec<u8>> {
        if offset.saturating_add(length as u64) > self.len() {
            return Err(Error::InvalidInput(format!(
                "Range [{offset}..{}] exceeds source length ({})",
                offset.saturating_add(length as u64),
                self.len()
            )));
        }
        match self {
            Self::Bytes { data, .. } => {
                #[allow(clippy::cast_possible_truncation)]
                let start = offset as usize;
                Ok(data[start..start + length].to_vec())
            }
            Self::File { path, .. } => {
                let mut file = File::open(path)?;
                file.seek(SeekFrom::Start(offset))?;
                let mut buf = vec![0u8; length];
                file.read_exact(&mut buf)?;
                Ok(buf)
            }
        }
    }

    /// Obtain the full document bytes, enforcing an explicit `max_bytes` ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MemoryLimitExceeded`] if `self.len() > max_bytes`.
    pub fn to_bytes_bounded(&self, max_bytes: usize) -> Result<Bytes> {
        if self.len() > max_bytes as u64 {
            #[allow(clippy::cast_possible_truncation)]
            let used = self.len().min(usize::MAX as u64) as usize;
            return Err(Error::MemoryLimitExceeded {
                used,
                limit: max_bytes,
            });
        }
        match self {
            Self::Bytes { data, .. } => Ok(data.clone()),
            Self::File { path, len } => {
                #[allow(clippy::cast_possible_truncation)]
                let mut buf = Vec::with_capacity(*len as usize);
                // The file may have grown since its length was taken, so the read itself is
                // bounded too.
                File::open(path)?
                    .take(max_bytes as u64 + 1)
                    .read_to_end(&mut buf)?;
                if buf.len() > max_bytes {
                    return Err(Error::MemoryLimitExceeded {
                        used: buf.len(),
                        limit: max_bytes,
                    });
                }
                Ok(Bytes::from(buf))
            }
        }
    }

    /// Detect the document format using only a bounded prefix (plus ZIP/CFB central-directory
    /// inspection when needed) so multi-GB files can be identified in `O(1)` memory.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if reading the file header fails.
    pub fn detect_format(&self) -> Result<Option<DetectionResult>> {
        let prefix = self.read_prefix(DEFAULT_DETECTION_PREFIX_BYTES)?;
        let fname = self.filename();
        if let Some(mut det) = detect_format(&prefix, fname) {
            // If the prefix matched generic ZIP (`application/zip`) on a large file where the
            // OOXML entry names live in the central directory at the end of the file, inspect
            // the tail of the file (last 64 KiB) to identify `.docx` / `.pptx` / `.xlsx`.
            if det.format.mime_type == "application/zip" && self.len() > prefix.len() as u64 {
                let tail_len = usize::try_from(self.len().min(65_536)).unwrap_or(65_536);
                let tail_offset = self.len().saturating_sub(tail_len as u64);
                if let Ok(tail) = self.read_range(tail_offset, tail_len) {
                    let mut combined = prefix;
                    combined.extend_from_slice(&tail);
                    if let Some(refined) = detect_format(&combined, fname) {
                        det = refined;
                    }
                }
            }
            return Ok(Some(det));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_source_bytes_and_file_random_access() {
        let content = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\n";
        let mem = DocumentSource::from_bytes(&content[..], Some("sample.pdf".to_string()));
        assert_eq!(mem.len(), content.len() as u64);
        assert_eq!(mem.filename(), Some("sample.pdf"));
        assert_eq!(mem.read_prefix(8).unwrap(), b"%PDF-1.7");
        assert_eq!(mem.read_range(9, 7).unwrap(), b"1 0 obj");
        let det = mem.detect_format().unwrap().expect("detected");
        assert_eq!(det.format.extension, "pdf");

        let tmp_path = std::env::temp_dir().join("prism_doc_source_test.pdf");
        std::fs::write(&tmp_path, content).unwrap();
        let file_src = DocumentSource::from_path(&tmp_path).unwrap();
        assert_eq!(file_src.len(), content.len() as u64);
        assert_eq!(file_src.filename(), Some("prism_doc_source_test.pdf"));
        assert_eq!(file_src.read_prefix(8).unwrap(), b"%PDF-1.7");
        assert_eq!(file_src.read_range(9, 7).unwrap(), b"1 0 obj");
        assert!(file_src.to_bytes_bounded(10).is_err());
        assert_eq!(file_src.to_bytes_bounded(1024).unwrap().as_ref(), content);
        let det_file = file_src.detect_format().unwrap().expect("detected file");
        assert_eq!(det_file.format.extension, "pdf");
        let _ = std::fs::remove_file(&tmp_path);
    }
}

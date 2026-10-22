// SPDX-License-Identifier: AGPL-3.0-only
//! Bounded decompression helpers to prevent decompression-bomb memory exhaustion.

use prism_core::{
    error::{Error, Result},
    parser::ParseContext,
};
use std::io::Read;

/// Default maximum decompressed size (64 MiB) for a stream or archive entry.
pub const DEFAULT_MAX_DECOMPRESSED_BYTES: usize = 64 * 1024 * 1024;

/// Return the effective decompressed byte limit from `context.options.max_memory`
/// or [`DEFAULT_MAX_DECOMPRESSED_BYTES`].
#[must_use]
pub fn max_decompressed_bytes(context: &ParseContext) -> usize {
    context
        .options
        .max_memory
        .unwrap_or(DEFAULT_MAX_DECOMPRESSED_BYTES)
}

/// Read all bytes from `reader`, failing with [`Error::MemoryLimitExceeded`] if
/// the stream produces more than `limit` bytes.
///
/// # Errors
///
/// Returns [`Error::MemoryLimitExceeded`] when the decompressed output exceeds `limit`,
/// or [`Error::ParseError`] on an underlying I/O error.
pub fn read_bounded<R: Read>(reader: R, limit: usize) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    read_to_end_with_limit(reader, &mut buf, limit)?;
    Ok(buf)
}

/// Read all UTF-8 text from `reader`, failing with [`Error::MemoryLimitExceeded`]
/// if the stream produces more than `limit` bytes.
///
/// # Errors
///
/// Returns [`Error::MemoryLimitExceeded`] when the decompressed output exceeds `limit`,
/// or [`Error::ParseError`] on I/O or UTF-8 decoding errors.
pub fn read_bounded_string<R: Read>(reader: R, limit: usize) -> Result<String> {
    let bytes = read_bounded(reader, limit)?;
    String::from_utf8(bytes)
        .map_err(|e| Error::ParseError(format!("Invalid UTF-8 in decompressed stream: {e}")))
}

/// Append bytes from `reader` into `buf` up to [`DEFAULT_MAX_DECOMPRESSED_BYTES`].
///
/// # Errors
///
/// Returns [`Error::MemoryLimitExceeded`] if more than [`DEFAULT_MAX_DECOMPRESSED_BYTES`]
/// bytes are read, or [`Error::ParseError`] on an I/O error.
pub fn read_to_end_bounded<R: Read>(reader: R, buf: &mut Vec<u8>) -> Result<usize> {
    read_to_end_with_limit(reader, buf, DEFAULT_MAX_DECOMPRESSED_BYTES)
}

/// Append bytes from `reader` into `buf` up to `limit`.
///
/// # Errors
///
/// Returns [`Error::MemoryLimitExceeded`] if more than `limit` bytes are read,
/// or [`Error::ParseError`] on an I/O error.
pub fn read_to_end_with_limit<R: Read>(
    reader: R,
    buf: &mut Vec<u8>,
    limit: usize,
) -> Result<usize> {
    let start_len = buf.len();
    let max_u64 = u64::try_from(limit).unwrap_or(u64::MAX);
    let read = reader
        .take(max_u64.saturating_add(1))
        .read_to_end(buf)
        .map_err(|e| Error::ParseError(format!("Decompression failed: {e}")))?;
    if read > limit {
        buf.truncate(start_len);
        return Err(Error::MemoryLimitExceeded { used: read, limit });
    }
    Ok(read)
}

/// Append UTF-8 text from `reader` into `buf` up to [`DEFAULT_MAX_DECOMPRESSED_BYTES`].
///
/// # Errors
///
/// Returns [`Error::MemoryLimitExceeded`] if more than [`DEFAULT_MAX_DECOMPRESSED_BYTES`]
/// bytes are read, or [`Error::ParseError`] on I/O or UTF-8 decoding errors.
pub fn read_to_string_bounded<R: Read>(reader: R, buf: &mut String) -> Result<usize> {
    read_to_string_with_limit(reader, buf, DEFAULT_MAX_DECOMPRESSED_BYTES)
}

/// Append UTF-8 text from `reader` into `buf` up to `limit`.
///
/// # Errors
///
/// Returns [`Error::MemoryLimitExceeded`] if more than `limit` bytes are read,
/// or [`Error::ParseError`] on I/O or UTF-8 decoding errors.
pub fn read_to_string_with_limit<R: Read>(
    reader: R,
    buf: &mut String,
    limit: usize,
) -> Result<usize> {
    let s = read_bounded_string(reader, limit)?;
    let len = s.len();
    buf.push_str(&s);
    Ok(len)
}

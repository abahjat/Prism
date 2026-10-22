// SPDX-License-Identifier: AGPL-3.0-only
//! C ABI for embedding Prism in native hosts (.NET, Java, C/C++).
//!
//! # Conventions
//!
//! - Input buffers are `(data, len)` pairs; `data` may only be null when `len` is 0.
//! - `filename` is an optional NUL-terminated UTF-8 string (may be null). It is
//!   used as a format-detection hint and as the document title fallback.
//! - Returned strings are NUL-terminated UTF-8, owned by the caller, and must be
//!   released with [`prism_free_string`]. A null return means failure; call
//!   [`prism_last_error`] on the same thread for the reason.
//! - No function unwinds into the caller: panics are caught and reported as errors.

use bytes::Bytes;
use libc::{c_char, size_t};
use prism_core::Document;
use prism_core::format::{DetectionResult, detect_format};
use prism_core::parser::{ParseContext, ParseOptions};
use prism_core::render::{RenderContext, Renderer};
use prism_parsers::registry::ParserRegistry;
use prism_render::html::HtmlRenderer;
use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::ptr;
use std::slice;
use std::sync::LazyLock;
use tokio::runtime::Runtime;

static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("prism-worker")
        .enable_all()
        .build()
        .expect("failed to start the Prism runtime")
});

static REGISTRY: LazyLock<ParserRegistry> = LazyLock::new(ParserRegistry::with_default_parsers);

static VERSION: LazyLock<CString> =
    LazyLock::new(|| CString::new(prism_core::FULL_VERSION).expect("version has no NUL bytes"));

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error(message: &str) {
    LAST_ERROR.with(|slot| *slot.borrow_mut() = Some(c_string_lossy(message)));
}

fn clear_last_error() {
    LAST_ERROR.with(|slot| *slot.borrow_mut() = None);
}

/// Build a C string, dropping interior NUL bytes (documents may contain them)
fn c_string_lossy(text: &str) -> CString {
    CString::new(text.replace('\0', "")).expect("NUL bytes removed")
}

/// Run `f`, converting errors and panics into a null return plus last-error
fn guarded(f: impl FnOnce() -> Result<String, String>) -> *mut c_char {
    clear_last_error();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(Ok(text)) => c_string_lossy(&text).into_raw(),
        Ok(Err(message)) => {
            set_last_error(&message);
            ptr::null_mut()
        }
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("unknown panic");
            set_last_error(&format!("internal error: {message}"));
            ptr::null_mut()
        }
    }
}

/// Borrow the input buffer
///
/// # Safety
///
/// `data` must point to `len` readable bytes, or `len` must be 0.
unsafe fn input<'a>(data: *const u8, len: size_t) -> Result<&'a [u8], String> {
    if len == 0 {
        return Ok(&[]);
    }
    if data.is_null() {
        return Err("data is null but len is non-zero".to_string());
    }
    // SAFETY: guaranteed by the caller
    Ok(unsafe { slice::from_raw_parts(data, len) })
}

/// Read the optional filename hint
///
/// # Safety
///
/// `filename` must be null or a valid NUL-terminated string.
unsafe fn filename_hint(filename: *const c_char) -> Option<String> {
    if filename.is_null() {
        return None;
    }
    // SAFETY: guaranteed by the caller
    let name = unsafe { CStr::from_ptr(filename) }
        .to_string_lossy()
        .into_owned();
    (!name.is_empty()).then_some(name)
}

/// Detect the format, using the filename's extension as a hint
fn detect(data: &[u8], filename: Option<&str>) -> Option<DetectionResult> {
    detect_format(data, filename)
}

/// Detect and parse a document
fn parse(data: &[u8], filename: Option<String>) -> Result<Document, String> {
    let detection = detect(data, filename.as_deref())
        .ok_or_else(|| "unrecognized document format".to_string())?;
    let parser = REGISTRY
        .get_parser(&detection.format)
        .ok_or_else(|| format!("no parser available for {}", detection.format.name))?;
    let context = ParseContext {
        format: detection.format.clone(),
        filename,
        size: data.len(),
        options: ParseOptions::default(),
    };
    RUNTIME
        .block_on(parser.parse(Bytes::copy_from_slice(data), context))
        .map_err(|e| format!("failed to parse {}: {e}", detection.format.name))
}

fn render_html(document: &Document) -> Result<String, String> {
    let context = RenderContext {
        options: Default::default(),
        filename: None,
    };
    let html = RUNTIME
        .block_on(HtmlRenderer::default().render(document, context))
        .map_err(|e| format!("failed to render HTML: {e}"))?;
    Ok(String::from_utf8_lossy(&html).into_owned())
}

/// Initialize logging (optional). Honors the `RUST_LOG` environment variable.
#[unsafe(no_mangle)]
pub extern "C" fn prism_init() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
}

/// Library version, e.g. `0.5.0+abc1234`. The string is static: do not free it.
#[unsafe(no_mangle)]
pub extern "C" fn prism_version() -> *const c_char {
    VERSION.as_ptr()
}

/// Message for the last failed call on this thread, or null if it succeeded.
///
/// The string is owned by the library and valid until the next Prism call on
/// the same thread: do not free it.
#[unsafe(no_mangle)]
pub extern "C" fn prism_last_error() -> *const c_char {
    LAST_ERROR.with(|slot| slot.borrow().as_ref().map_or(ptr::null(), |s| s.as_ptr()))
}

/// Free a string returned by the library
///
/// # Safety
///
/// The pointer `s` must have been returned by a function in this library
/// (other than [`prism_version`] and [`prism_last_error`]) and not freed before.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prism_free_string(s: *mut c_char) {
    if !s.is_null() {
        // SAFETY: guaranteed by the caller
        drop(unsafe { CString::from_raw(s) });
    }
}

/// Detect the format of a buffer. Returns the format as JSON.
///
/// # Safety
///
/// `data` must point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prism_detect_format(data: *const u8, len: size_t) -> *mut c_char {
    // SAFETY: forwarded caller guarantees
    unsafe { prism_detect_format_ex(data, len, ptr::null()) }
}

/// Detect the format of a buffer with a filename hint. Returns JSON with the
/// format and detection confidence.
///
/// # Safety
///
/// `data` must point to `len` readable bytes; `filename` must be null or a
/// NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prism_detect_format_ex(
    data: *const u8,
    len: size_t,
    filename: *const c_char,
) -> *mut c_char {
    guarded(|| {
        // SAFETY: forwarded caller guarantees
        let (data, filename) = unsafe { (input(data, len)?, filename_hint(filename)) };
        let detection = detect(data, filename.as_deref())
            .ok_or_else(|| "unrecognized document format".to_string())?;
        let json = serde_json::json!({
            "format": detection.format,
            "confidence": detection.confidence,
        });
        Ok(json.to_string())
    })
}

/// Convert a document to a self-contained HTML page.
///
/// # Safety
///
/// `data` must point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prism_convert_to_html(data: *const u8, len: size_t) -> *mut c_char {
    // SAFETY: forwarded caller guarantees
    unsafe { prism_convert_to_html_ex(data, len, ptr::null()) }
}

/// Convert a document to a self-contained HTML page, with a filename hint.
///
/// The page loads nothing from the network and runs no document scripts.
///
/// # Safety
///
/// `data` must point to `len` readable bytes; `filename` must be null or a
/// NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prism_convert_to_html_ex(
    data: *const u8,
    len: size_t,
    filename: *const c_char,
) -> *mut c_char {
    guarded(|| {
        // SAFETY: forwarded caller guarantees
        let (data, filename) = unsafe { (input(data, len)?, filename_hint(filename)) };
        render_html(&parse(data, filename)?)
    })
}

/// Extract the plain text of a document (for indexing and search).
///
/// # Safety
///
/// `data` must point to `len` readable bytes; `filename` must be null or a
/// NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prism_extract_text(
    data: *const u8,
    len: size_t,
    filename: *const c_char,
) -> *mut c_char {
    guarded(|| {
        // SAFETY: forwarded caller guarantees
        let (data, filename) = unsafe { (input(data, len)?, filename_hint(filename)) };
        Ok(parse(data, filename)?.extract_text())
    })
}

/// Extract document metadata as JSON: format, page count, attachment names
/// and the document properties.
///
/// # Safety
///
/// `data` must point to `len` readable bytes; `filename` must be null or a
/// NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prism_extract_metadata(
    data: *const u8,
    len: size_t,
    filename: *const c_char,
) -> *mut c_char {
    guarded(|| {
        // SAFETY: forwarded caller guarantees
        let (data, filename) = unsafe { (input(data, len)?, filename_hint(filename)) };
        let format = detect(data, filename.as_deref()).map(|d| d.format);
        let document = parse(data, filename)?;
        let attachments: Vec<_> = document
            .attachments
            .iter()
            .map(|a| {
                serde_json::json!({
                    "filename": a.filename,
                    "mime_type": a.mime_type,
                    "size": a.data.len(),
                })
            })
            .collect();
        let json = serde_json::json!({
            "format": format,
            "page_count": document.page_count(),
            "metadata": document.metadata,
            "attachments": attachments,
        });
        Ok(json.to_string())
    })
}

/// Text preview of a buffer: the first 2000 characters of its text, or a hex
/// dump when no text can be extracted. Never fails for valid input.
///
/// # Safety
///
/// `data` must point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prism_preview_file(data: *const u8, len: size_t) -> *mut c_char {
    guarded(|| {
        // SAFETY: forwarded caller guarantees
        let data = unsafe { input(data, len)? };
        let text = parse(data, None)
            .map(|doc| doc.extract_text())
            .unwrap_or_default();
        if text.trim().is_empty() {
            return Ok(hex_dump(data));
        }
        let mut preview: String = text.chars().take(2000).collect();
        if text.chars().nth(2000).is_some() {
            preview.push_str("\n... (truncated)");
        }
        Ok(preview)
    })
}

fn hex_dump(data: &[u8]) -> String {
    use std::fmt::Write;
    let mut hex = String::from("Binary file - Hex dump:\n");
    for (i, chunk) in data[..data.len().min(256)].chunks(16).enumerate() {
        let _ = write!(hex, "{:04X}: ", i * 16);
        for byte in chunk {
            let _ = write!(hex, "{byte:02X} ");
        }
        hex.push('\n');
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    fn take(ptr: *mut c_char) -> Option<String> {
        if ptr.is_null() {
            return None;
        }
        let text = unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned();
        unsafe { prism_free_string(ptr) };
        Some(text)
    }

    fn last_error() -> Option<String> {
        let ptr = prism_last_error();
        (!ptr.is_null()).then(|| {
            unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned()
        })
    }

    #[test]
    fn html_and_text_from_markdown() {
        let data = b"# Title\n\nSome **bold** text";
        let name = CString::new("notes.md").unwrap();
        let html =
            take(unsafe { prism_convert_to_html_ex(data.as_ptr(), data.len(), name.as_ptr()) })
                .expect("html");
        assert!(html.contains("Content-Security-Policy"));
        let text = take(unsafe { prism_extract_text(data.as_ptr(), data.len(), name.as_ptr()) })
            .expect("text");
        assert_eq!(text, "Title\nSome bold text");
        assert!(last_error().is_none());
    }

    #[test]
    fn failures_set_last_error() {
        let result = take(unsafe { prism_extract_text(ptr::null(), 4, ptr::null()) });
        assert!(result.is_none());
        assert!(last_error().unwrap().contains("null"));
    }

    #[test]
    fn version_is_static() {
        let version = unsafe { CStr::from_ptr(prism_version()) }.to_str().unwrap();
        assert!(version.starts_with(env!("CARGO_PKG_VERSION")));
    }
}

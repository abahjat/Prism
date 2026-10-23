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
use prism_layout::{
    LayoutDocument, LayoutOptions, SearchOptions, interpret_pdf_bytes, layout_document,
};
use prism_parsers::registry::ParserRegistry;
use prism_render::html::HtmlRenderer;
use prism_render::pdf::{PdfExportOptions, export_document_pdf};
use prism_render::raster::{RasterOptions, export_document_tiff, render_page_png};
use prism_render::svg::{
    SvgViewerOptions, render_document_viewer_html, render_page_svg, render_page_viewer_fragment,
};
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

// ============================================================================
// Page-on-demand C ABI (`PrismDocument`)
// ============================================================================

/// Page render output format: standalone UTF-8 SVG document (`<svg ...>...</svg>`).
pub const PRISM_PAGE_FORMAT_SVG: u32 = 0;
/// Page render output format: 8-bit RGBA PNG image bytes.
pub const PRISM_PAGE_FORMAT_PNG: u32 = 1;
/// Page render output format: UTF-8 HTML `<section class="prism-page">` fragment with SVG + text layer.
pub const PRISM_PAGE_FORMAT_VIEWER_FRAGMENT: u32 = 2;

/// Document export format: searchable PDF 1.7 document bytes.
pub const PRISM_EXPORT_FORMAT_PDF: u32 = 0;
/// Document export format: multi-page RGBA TIFF document bytes.
pub const PRISM_EXPORT_FORMAT_TIFF: u32 = 1;
/// Document export format: self-contained HTML5 viewer document bytes.
pub const PRISM_EXPORT_FORMAT_HTML_VIEWER: u32 = 2;

/// Search flag bit 0: case-sensitive matching.
pub const PRISM_SEARCH_CASE_SENSITIVE: u32 = 1 << 0;
/// Search flag bit 1: ignore combining diacritical marks (accents).
pub const PRISM_SEARCH_IGNORE_DIACRITICS: u32 = 1 << 1;
/// Search flag bit 2: match whole words only.
pub const PRISM_SEARCH_WHOLE_WORD: u32 = 1 << 2;

/// Opaque handle to a parsed and positioned Prism document (`LayoutDocument`).
pub struct PrismDocument {
    layout: LayoutDocument,
    title: Option<String>,
}

/// Run a fallible closure returning `i32` (`0` on success, `-1` on error/panic).
fn guarded_status(f: impl FnOnce() -> Result<(), String>) -> i32 {
    clear_last_error();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(Ok(())) => 0,
        Ok(Err(message)) => {
            set_last_error(&message);
            -1
        }
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("unknown panic");
            set_last_error(&format!("internal error: {message}"));
            -1
        }
    }
}

/// Allocate a caller-owned byte buffer from a `Vec<u8>` so [`prism_buffer_free`] can reclaim it.
fn into_raw_buffer(mut vec: Vec<u8>) -> (*mut u8, size_t) {
    vec.shrink_to_fit();
    let boxed: Box<[u8]> = vec.into_boxed_slice();
    let len = boxed.len();
    let ptr = Box::into_raw(boxed).cast::<u8>();
    (ptr, len)
}

/// Open a document from an in-memory buffer and lay out its pages into a [`PrismDocument`].
///
/// Returns a non-null handle on success, or null on failure (check [`prism_last_error`]).
/// Free the returned handle with [`prism_document_close`].
///
/// # Safety
///
/// `data` must point to `len` readable bytes; `filename` must be null or a valid
/// NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prism_document_open(
    data: *const u8,
    len: size_t,
    filename: *const c_char,
) -> *mut PrismDocument {
    clear_last_error();
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> Result<PrismDocument, String> {
            // SAFETY: forwarded caller guarantees
            let (bytes, filename_opt) = unsafe { (input(data, len)?, filename_hint(filename)) };
            let options = LayoutOptions::default();

            // Fast path for PDF buffers
            if bytes.starts_with(b"%PDF-")
                && let Ok(layout) = interpret_pdf_bytes(bytes, &options)
            {
                return Ok(PrismDocument {
                    layout,
                    title: filename_opt,
                });
            }

            let udm = parse(bytes, filename_opt.clone())?;
            let title = udm.metadata.title.clone().or(filename_opt);
            let layout = layout_document(&udm, &options).map_err(|e| match e {
                prism_core::Error::UnsupportedFormat(_) => {
                    format!("layout failed: {e}; use prism_convert_to_html for this document")
                }
                _ => format!("layout failed: {e}"),
            })?;
            Ok(PrismDocument { layout, title })
        },
    ));

    match res {
        Ok(Ok(doc)) => Box::into_raw(Box::new(doc)),
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

/// Close and free a [`PrismDocument`] handle returned by [`prism_document_open`].
///
/// Passing null is a safe no-op.
///
/// # Safety
///
/// `doc` must be null or a valid pointer returned by [`prism_document_open`] that has
/// not yet been closed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prism_document_close(doc: *mut PrismDocument) {
    if !doc.is_null() {
        // SAFETY: guaranteed by caller
        drop(unsafe { Box::from_raw(doc) });
    }
}

/// Return the number of laid-out pages in `doc`, or `0` if `doc` is null.
///
/// # Safety
///
/// `doc` must be null or a valid pointer to a [`PrismDocument`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prism_document_page_count(doc: *const PrismDocument) -> u32 {
    if doc.is_null() {
        set_last_error("doc is null");
        return 0;
    }
    // SAFETY: guaranteed by caller
    let doc_ref = unsafe { &*doc };
    u32::try_from(doc_ref.layout.pages.len()).unwrap_or(0)
}

/// Query the size of a 0-based `page` in points (`1/72` inch).
///
/// Returns `0` on success and writes `width` and `height` to `w_pt` and `h_pt`.
/// Returns `-1` on failure and sets [`prism_last_error`].
///
/// # Safety
///
/// `doc` must be a valid [`PrismDocument`] pointer; `w_pt` and `h_pt` must be valid,
/// non-null writable pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prism_document_page_size(
    doc: *const PrismDocument,
    page: u32,
    w_pt: *mut f32,
    h_pt: *mut f32,
) -> i32 {
    guarded_status(|| {
        if doc.is_null() {
            return Err("doc is null".to_string());
        }
        if w_pt.is_null() || h_pt.is_null() {
            return Err("w_pt or h_pt output pointer is null".to_string());
        }
        // SAFETY: guaranteed by caller
        let doc_ref = unsafe { &*doc };
        let layout_page = doc_ref.layout.pages.get(page as usize).ok_or_else(|| {
            format!(
                "page index {page} out of range (page_count = {})",
                doc_ref.layout.pages.len()
            )
        })?;
        // SAFETY: checked non-null above
        unsafe {
            *w_pt = layout_page.size.width;
            *h_pt = layout_page.size.height;
        }
        Ok(())
    })
}

/// Render a single 0-based `page` to SVG (`0`), PNG (`1`), or HTML viewer fragment (`2`).
///
/// On success, writes the newly allocated buffer pointer and length to `out_buf` and `out_len`
/// and returns `0`. The caller must release the buffer using [`prism_buffer_free`].
/// On failure, returns `-1` and sets [`prism_last_error`].
///
/// # Safety
///
/// `doc` must be a valid [`PrismDocument`] pointer; `out_buf` and `out_len` must be valid,
/// non-null writable pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prism_document_render_page(
    doc: *const PrismDocument,
    page: u32,
    format: u32,
    dpi: f32,
    out_buf: *mut *mut u8,
    out_len: *mut size_t,
) -> i32 {
    guarded_status(|| {
        if doc.is_null() {
            return Err("doc is null".to_string());
        }
        if out_buf.is_null() || out_len.is_null() {
            return Err("out_buf or out_len pointer is null".to_string());
        }
        // SAFETY: guaranteed by caller
        let doc_ref = unsafe { &*doc };
        let layout_page = doc_ref.layout.pages.get(page as usize).ok_or_else(|| {
            format!(
                "page index {page} out of range (page_count = {})",
                doc_ref.layout.pages.len()
            )
        })?;

        let opts = LayoutOptions::default();
        let bytes = match format {
            PRISM_PAGE_FORMAT_SVG => render_page_svg(
                layout_page,
                &doc_ref.layout.fonts,
                &doc_ref.layout.images,
                &opts,
            )
            .map(String::into_bytes)
            .map_err(|e| format!("SVG render failed: {e}"))?,
            PRISM_PAGE_FORMAT_PNG => {
                let raster_opts = RasterOptions {
                    dpi: if dpi > 0.0 { dpi } else { 150.0 },
                    layout_options: opts,
                    ..RasterOptions::default()
                };
                render_page_png(
                    layout_page,
                    &doc_ref.layout.fonts,
                    &doc_ref.layout.images,
                    &raster_opts,
                )
                .map_err(|e| format!("PNG render failed: {e}"))?
            }
            PRISM_PAGE_FORMAT_VIEWER_FRAGMENT => render_page_viewer_fragment(
                layout_page,
                &doc_ref.layout.fonts,
                &doc_ref.layout.images,
                &opts,
            )
            .map(String::into_bytes)
            .map_err(|e| format!("viewer fragment render failed: {e}"))?,
            other => return Err(format!("unsupported page render format: {other}")),
        };

        let (ptr, len) = into_raw_buffer(bytes);
        // SAFETY: checked non-null above
        unsafe {
            *out_buf = ptr;
            *out_len = len;
        }
        Ok(())
    })
}

/// Extract reading-order plain text for a single 0-based `page`.
///
/// Returns a NUL-terminated UTF-8 string that must be freed with [`prism_free_string`],
/// or null on error (check [`prism_last_error`]).
///
/// # Safety
///
/// `doc` must be a valid [`PrismDocument`] pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prism_document_page_text(
    doc: *const PrismDocument,
    page: u32,
) -> *mut c_char {
    guarded(|| {
        if doc.is_null() {
            return Err("doc is null".to_string());
        }
        // SAFETY: guaranteed by caller
        let doc_ref = unsafe { &*doc };
        let layout_page = doc_ref.layout.pages.get(page as usize).ok_or_else(|| {
            format!(
                "page index {page} out of range (page_count = {})",
                doc_ref.layout.pages.len()
            )
        })?;
        Ok(layout_page.text())
    })
}

/// Search across all pages of `doc` for `query` and return matching hits + bounding boxes as JSON.
///
/// `flags` is a bitmask of [`PRISM_SEARCH_CASE_SENSITIVE`], [`PRISM_SEARCH_IGNORE_DIACRITICS`],
/// and [`PRISM_SEARCH_WHOLE_WORD`].
/// Returns a NUL-terminated UTF-8 JSON string that must be freed with [`prism_free_string`],
/// or null on error.
///
/// # Safety
///
/// `doc` must be a valid [`PrismDocument`] pointer; `query` must be a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prism_document_search(
    doc: *const PrismDocument,
    query: *const c_char,
    flags: u32,
) -> *mut c_char {
    guarded(|| {
        if doc.is_null() {
            return Err("doc is null".to_string());
        }
        if query.is_null() {
            return Err("query is null".to_string());
        }
        // SAFETY: guaranteed by caller
        let doc_ref = unsafe { &*doc };
        let query_str = unsafe { CStr::from_ptr(query) }.to_string_lossy();
        let options = SearchOptions {
            case_sensitive: (flags & PRISM_SEARCH_CASE_SENSITIVE) != 0,
            ignore_diacritics: (flags & PRISM_SEARCH_IGNORE_DIACRITICS) != 0,
            whole_word: (flags & PRISM_SEARCH_WHOLE_WORD) != 0,
        };
        let hits = doc_ref.layout.find(&query_str, options);
        serde_json::to_string(&hits).map_err(|e| format!("failed to serialize search hits: {e}"))
    })
}

/// Export the entire [`PrismDocument`] to PDF (`0`), multi-page TIFF (`1`), or HTML viewer (`2`).
///
/// On success, writes the allocated buffer pointer and length to `out_buf` and `out_len`
/// and returns `0`. Release the buffer with [`prism_buffer_free`].
///
/// # Safety
///
/// `doc` must be a valid [`PrismDocument`] pointer; `out_buf` and `out_len` must be valid,
/// non-null writable pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prism_document_export(
    doc: *const PrismDocument,
    format: u32,
    out_buf: *mut *mut u8,
    out_len: *mut size_t,
) -> i32 {
    guarded_status(|| {
        if doc.is_null() {
            return Err("doc is null".to_string());
        }
        if out_buf.is_null() || out_len.is_null() {
            return Err("out_buf or out_len pointer is null".to_string());
        }
        // SAFETY: guaranteed by caller
        let doc_ref = unsafe { &*doc };
        let bytes = match format {
            PRISM_EXPORT_FORMAT_PDF => {
                export_document_pdf(&doc_ref.layout, &PdfExportOptions::default())
                    .map_err(|e| format!("PDF export failed: {e}"))?
            }
            PRISM_EXPORT_FORMAT_TIFF => {
                export_document_tiff(&doc_ref.layout, &RasterOptions::default())
                    .map_err(|e| format!("TIFF export failed: {e}"))?
            }
            PRISM_EXPORT_FORMAT_HTML_VIEWER => {
                let viewer_opts = SvgViewerOptions {
                    title: doc_ref.title.clone(),
                    white_background: true,
                    layout_options: LayoutOptions::default(),
                };
                render_document_viewer_html(&doc_ref.layout, &viewer_opts)
                    .map(String::into_bytes)
                    .map_err(|e| format!("HTML viewer export failed: {e}"))?
            }
            other => return Err(format!("unsupported export format: {other}")),
        };

        let (ptr, len) = into_raw_buffer(bytes);
        // SAFETY: checked non-null above
        unsafe {
            *out_buf = ptr;
            *out_len = len;
        }
        Ok(())
    })
}

/// Return the per-document font resolution and substitution report as JSON.
///
/// Free the returned string with [`prism_free_string`].
///
/// # Safety
///
/// `doc` must be a valid [`PrismDocument`] pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prism_document_font_report(doc: *const PrismDocument) -> *mut c_char {
    guarded(|| {
        if doc.is_null() {
            return Err("doc is null".to_string());
        }
        // SAFETY: guaranteed by caller
        let doc_ref = unsafe { &*doc };
        serde_json::to_string(&doc_ref.layout.font_report)
            .map_err(|e| format!("failed to serialize font report: {e}"))
    })
}

/// Free a byte buffer returned by [`prism_document_render_page`] or [`prism_document_export`].
///
/// Passing a null `buf` is a safe no-op.
///
/// # Safety
///
/// `buf` and `len` must match a buffer previously returned by [`prism_document_render_page`]
/// or [`prism_document_export`] and not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prism_buffer_free(buf: *mut u8, len: size_t) {
    if !buf.is_null() {
        // SAFETY: guaranteed by caller
        let slice_ptr = ptr::slice_from_raw_parts_mut(buf, len);
        drop(unsafe { Box::from_raw(slice_ptr) });
    }
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

    #[test]
    fn page_on_demand_c_abi_lifecycle() {
        let data = b"Hello from Prism page-on-demand C ABI!\nSecond paragraph of text.";
        let name = CString::new("sample.txt").unwrap();
        let doc = unsafe { prism_document_open(data.as_ptr(), data.len(), name.as_ptr()) };
        assert!(!doc.is_null(), "open failed: {:?}", last_error());

        let page_count = unsafe { prism_document_page_count(doc) };
        assert_eq!(page_count, 1);

        let mut w_pt = 0.0_f32;
        let mut h_pt = 0.0_f32;
        let rc = unsafe { prism_document_page_size(doc, 0, &raw mut w_pt, &raw mut h_pt) };
        assert_eq!(rc, 0);
        assert!(w_pt > 100.0 && h_pt > 100.0);

        // Render page as SVG
        let mut buf: *mut u8 = ptr::null_mut();
        let mut len: size_t = 0;
        let rc_svg = unsafe {
            prism_document_render_page(
                doc,
                0,
                PRISM_PAGE_FORMAT_SVG,
                96.0,
                &raw mut buf,
                &raw mut len,
            )
        };
        assert_eq!(rc_svg, 0);
        assert!(!buf.is_null() && len > 0);
        let svg_str =
            String::from_utf8(unsafe { slice::from_raw_parts(buf, len) }.to_vec()).unwrap();
        assert!(svg_str.contains("<svg"));
        unsafe { prism_buffer_free(buf, len) };

        // Render page as PNG
        let mut png_buf: *mut u8 = ptr::null_mut();
        let mut png_len: size_t = 0;
        let rc_png = unsafe {
            prism_document_render_page(
                doc,
                0,
                PRISM_PAGE_FORMAT_PNG,
                72.0,
                &raw mut png_buf,
                &raw mut png_len,
            )
        };
        assert_eq!(rc_png, 0);
        let png_slice = unsafe { slice::from_raw_parts(png_buf, png_len) };
        assert!(png_slice.starts_with(b"\x89PNG\r\n\x1a\n"));
        unsafe { prism_buffer_free(png_buf, png_len) };

        // Page text
        let page_text = take(unsafe { prism_document_page_text(doc, 0) }).expect("page text");
        assert!(page_text.contains("Hello from Prism"));

        // Search
        let query = CString::new("Prism").unwrap();
        let hits_json =
            take(unsafe { prism_document_search(doc, query.as_ptr(), 0) }).expect("search json");
        assert!(hits_json.contains("\"page\":0"));

        // Font report
        let font_report =
            take(unsafe { prism_document_font_report(doc) }).expect("font report json");
        assert!(font_report.contains("entries"));

        // Export PDF
        let mut pdf_buf: *mut u8 = ptr::null_mut();
        let mut pdf_len: size_t = 0;
        let rc_pdf = unsafe {
            prism_document_export(
                doc,
                PRISM_EXPORT_FORMAT_PDF,
                &raw mut pdf_buf,
                &raw mut pdf_len,
            )
        };
        assert_eq!(rc_pdf, 0);
        let pdf_slice = unsafe { slice::from_raw_parts(pdf_buf, pdf_len) };
        assert!(pdf_slice.starts_with(b"%PDF-"));
        unsafe { prism_buffer_free(pdf_buf, pdf_len) };

        unsafe { prism_document_close(doc) };
    }
}

use bytes::Bytes;
use libc::{c_char, size_t};
use prism_core::format::detect_format;
use prism_core::parser::{ParseContext, ParseOptions};
use prism_core::render::{RenderContext, Renderer};
use prism_render::html::HtmlRenderer;
use std::ffi::CString;
use std::ptr;
use std::slice;
use tokio::runtime::Builder;

// Initialize logging (optional)
#[no_mangle]
pub extern "C" fn prism_init() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
}

/// Detect format from raw bytes
///
/// # Safety
///
/// The `data` pointer must point to a valid memory region of size `len`.
#[no_mangle]
pub unsafe extern "C" fn prism_detect_format(data: *const u8, len: size_t) -> *mut c_char {
    let result = std::panic::catch_unwind(|| {
        let slice = unsafe { slice::from_raw_parts(data, len) };

        // We can't determine filename from raw bytes easily, passing None
        match detect_format(slice, None) {
            Some(fmt) => {
                let json = serde_json::to_string(&fmt.format).unwrap_or_default();
                CString::new(json).unwrap().into_raw()
            }
            None => ptr::null_mut(),
        }
    });

    match result {
        Ok(ptr) => ptr,
        Err(_) => ptr::null_mut(),
    }
}

/// Free a string returned by the library
///
/// # Safety
///
/// The pointer `s` must have been returned by a function in this library.
#[no_mangle]
pub unsafe extern "C" fn prism_free_string(s: *mut c_char) {
    let _ = std::panic::catch_unwind(|| {
        if s.is_null() {
            return;
        }
        unsafe {
            let _ = CString::from_raw(s);
        }
    });
}

/// Generate a preview string for a file buffer
///
/// # Safety
///
/// The `data` pointer must point to a valid memory region of size `len`.
#[no_mangle]
pub unsafe extern "C" fn prism_preview_file(data: *const u8, len: size_t) -> *mut c_char {
    let result = std::panic::catch_unwind(|| {
        let slice = unsafe { slice::from_raw_parts(data, len) };
        let bytes = Bytes::copy_from_slice(slice);

        // Detect format
        let detection = match detect_format(slice, None) {
            Some(d) => d,
            None => return make_hex_dump(slice),
        };

        // Get parser from registry
        let registry = prism_parsers::registry::ParserRegistry::with_default_parsers();
        let parser = match registry.get_parser(&detection.format) {
            Some(p) => p,
            None => return make_hex_dump(slice),
        };

        // Parse and extract text
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        let result = rt.block_on(async {
            let parse_context = ParseContext {
                format: detection.format,
                filename: None,
                size: len,
                options: ParseOptions::default(),
            };

            match parser.parse(bytes, parse_context).await {
                Ok(doc) => {
                    // Extract text from the first 5 pages or 2000 chars
                    let mut text = doc.extract_text();
                    if text.trim().is_empty() {
                        return None;
                    }
                    if text.chars().count() > 2000 {
                        text = text.chars().take(2000).collect();
                        text.push_str("\n... (truncated)");
                    }
                    Some(CString::new(text).unwrap().into_raw())
                }
                Err(_) => None,
            }
        });

        match result {
            Some(ptr) => ptr,
            None => make_hex_dump(slice),
        }
    });

    match result {
        Ok(ptr) => ptr,
        Err(_) => ptr::null_mut(),
    }
}

fn make_hex_dump(slice: &[u8]) -> *mut c_char {
    let max_bytes = slice.len().min(256);
    let mut hex = String::from("Binary file - Hex dump:\n");
    for (i, chunk) in slice[..max_bytes].chunks(16).enumerate() {
        hex.push_str(&format!("{:04X}: ", i * 16));
        for byte in chunk {
            hex.push_str(&format!("{:02X} ", byte));
        }
        hex.push('\n');
    }
    CString::new(hex).unwrap().into_raw()
}

/// Convert a file to HTML string
///
/// # Safety
///
/// The `data` pointer must point to a valid memory region of size `len`.
#[no_mangle]
pub unsafe extern "C" fn prism_convert_to_html(data: *const u8, len: size_t) -> *mut c_char {
    let result = std::panic::catch_unwind(|| {
        let slice = unsafe { slice::from_raw_parts(data, len) };
        let bytes = Bytes::copy_from_slice(slice);

        let detection = detect_format(slice, None);
        if detection.is_none() {
            return ptr::null_mut();
        }
        let format = detection.unwrap().format;

        // Use registry to get parser
        let registry = prism_parsers::registry::ParserRegistry::with_default_parsers();
        let parser = match registry.get_parser(&format) {
            Some(p) => p,
            None => return ptr::null_mut(),
        };

        // Create runtime for async execution
        // Note: Creating a new runtime for every call is heavy. In a real app, use a global runtime or OnceCell.
        let rt = Builder::new_current_thread().enable_all().build().unwrap();

        let result = rt.block_on(async {
            let parse_context = ParseContext {
                format: format.clone(),
                filename: None,
                size: len,
                options: ParseOptions::default(),
            };
            let doc = parser.parse(bytes, parse_context).await?;
            let renderer = HtmlRenderer::default();
            let context = RenderContext {
                options: Default::default(),
                filename: None,
            };
            let output = renderer.render(&doc, context).await?;
            Ok::<Vec<u8>, prism_core::error::Error>(output.to_vec())
        });

        match result {
            Ok(html_bytes) => {
                let s = String::from_utf8_lossy(&html_bytes).to_string();
                CString::new(s).unwrap().into_raw()
            }
            Err(_) => ptr::null_mut(),
        }
    });

    match result {
        Ok(ptr) => ptr,
        Err(_) => ptr::null_mut(),
    }
}

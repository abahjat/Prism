use bytes::Bytes;
use libc::{c_char, size_t};
use prism_core::format::detect_format;
use prism_core::parser::{ParseContext, ParseOptions, Parser};
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

        // Simple text extraction logic
        // In a real scenario, we'd use the parsers, but for now reuse the logic
        // we added to prism-server/src/convert.rs or simplified version here.

        // For now, let's just do a simple text check
        if let Ok(text) = std::str::from_utf8(slice) {
            let preview: String = text.chars().take(1000).collect();
            return CString::new(preview).unwrap().into_raw();
        }

        // Binary fallback (hex dump)
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
    });

    match result {
        Ok(ptr) => ptr,
        Err(_) => ptr::null_mut(),
    }
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
        // Copy because async parsers might need ownership or 'static
        // and using the raw pointer across await points is dangerous if the caller frees it (though block_on holds it).
        // Safest is to copy.
        let bytes = Bytes::copy_from_slice(slice);

        let detection = detect_format(slice, None);
        if detection.is_none() {
            return ptr::null_mut();
        }
        let format = detection.unwrap().format;
        let mime = format.mime_type.as_str();

        // Select parser
        let parser: Box<dyn Parser> = match mime {
            "application/pdf" => Box::new(prism_parsers::pdf::PdfParser::new()),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
                Box::new(prism_parsers::office::DocxParser::new())
            }
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
                Box::new(prism_parsers::office::XlsxParser::new())
            }
            "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
                Box::new(prism_parsers::office::PptxParser::new())
            }
            // Legacy Office
            "application/msword" => Box::new(prism_parsers::office::legacy::DocParser::new()),
            "application/vnd.ms-excel" => Box::new(prism_parsers::office::legacy::XlsParser::new()),
            "application/vnd.ms-powerpoint" => {
                Box::new(prism_parsers::office::legacy::PptParser::new())
            }
            "application/vnd.ms-project" => {
                Box::new(prism_parsers::office::legacy::MppParser::new())
            }
            // Text
            "text/plain" => Box::new(prism_parsers::text::TextParser::new()),
            "text/html" => Box::new(prism_parsers::text::HtmlParser::new()),
            "text/csv" => Box::new(prism_parsers::text::CsvParser::new()),
            "multipart/related" => Box::new(prism_parsers::email::MhtParser::new()),
            "application/epub+zip" => Box::new(prism_parsers::text::EpubParser::new()),
            "application/vnd.ms-xpsdocument" | "application/oxps" => {
                Box::new(prism_parsers::office::XpsParser::new())
            }
            // Fallback for generic OLE CFB to let legacy parsers try
            "application/x-cfb" => {
                // If we detect CFB, we can try to guess or just return null if we can't dispatch.
                // Since detect_format now handles specific legacy formats, x-cfb likely means unknown OLE.
                return ptr::null_mut();
            }
            _ => return ptr::null_mut(),
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

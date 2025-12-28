use libc::{c_char, size_t};
use prism_core::format::detect_format;
use std::ffi::CString;
use std::ptr;
use std::slice;

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
    let slice = unsafe { slice::from_raw_parts(data, len) };

    // We can't determine filename from raw bytes easily, passing None
    match detect_format(slice, None) {
        Some(fmt) => {
            let json = serde_json::to_string(&fmt.format).unwrap_or_default();
            CString::new(json).unwrap().into_raw()
        }
        None => ptr::null_mut(),
    }
}

/// Free a string returned by the library
///
/// # Safety
///
/// The pointer `s` must have been returned by a function in this library.
#[no_mangle]
pub unsafe extern "C" fn prism_free_string(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    unsafe {
        let _ = CString::from_raw(s);
    }
}

/// Generate a preview string for a file buffer
///
/// # Safety
///
/// The `data` pointer must point to a valid memory region of size `len`.
#[no_mangle]
pub unsafe extern "C" fn prism_preview_file(data: *const u8, len: size_t) -> *mut c_char {
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
}

//! # Format Detection
//!
//! Utilities for detecting document formats from file content.
//!
//! Format detection uses multiple strategies:
//! 1. Magic bytes / file signatures
//! 2. File extension hints
//! 3. Content analysis
//!
//! ## Example
//!
//! ```rust
//! use prism_core::format::{detect_format, Format};
//!
//! let data = include_bytes!("../tests/fixtures/sample.pdf");
//! let format = detect_format(data, Some("sample.pdf"));
//!
//! assert!(matches!(format, Some(Format::Pdf)));
//! ```

use serde::{Deserialize, Serialize};

/// Detected file format
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Format {
    /// MIME type (e.g., "application/pdf")
    pub mime_type: String,

    /// Common file extension (e.g., "pdf")
    pub extension: String,

    /// Format family
    pub family: FormatFamily,

    /// Human-readable name
    pub name: String,

    /// Whether this format can contain other files
    pub is_container: bool,
}

impl Format {
    // =========================================
    // Common format constants
    // =========================================

    /// PDF format
    pub const PDF: Format = Format {
        mime_type: String::new(), // Will be set properly in const fn when stabilized
        extension: String::new(),
        family: FormatFamily::Document,
        name: String::new(),
        is_container: false,
    };

    /// Create a new PDF format instance
    #[must_use]
    pub fn pdf() -> Self {
        Self {
            mime_type: "application/pdf".to_string(),
            extension: "pdf".to_string(),
            family: FormatFamily::Document,
            name: "PDF".to_string(),
            is_container: false,
        }
    }

    /// Create a new DOCX format instance
    #[must_use]
    pub fn docx() -> Self {
        Self {
            mime_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                .to_string(),
            extension: "docx".to_string(),
            family: FormatFamily::Office,
            name: "Microsoft Word (DOCX)".to_string(),
            is_container: true,
        }
    }

    /// Create a new XLSX format instance
    #[must_use]
    pub fn xlsx() -> Self {
        Self {
            mime_type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
                .to_string(),
            extension: "xlsx".to_string(),
            family: FormatFamily::Office,
            name: "Microsoft Excel (XLSX)".to_string(),
            is_container: true,
        }
    }

    /// Create a new PPTX format instance
    #[must_use]
    pub fn pptx() -> Self {
        Self {
            mime_type:
                "application/vnd.openxmlformats-officedocument.presentationml.presentation"
                    .to_string(),
            extension: "pptx".to_string(),
            family: FormatFamily::Office,
            name: "Microsoft PowerPoint (PPTX)".to_string(),
            is_container: true,
        }
    }

    /// Create a new PNG format instance
    #[must_use]
    pub fn png() -> Self {
        Self {
            mime_type: "image/png".to_string(),
            extension: "png".to_string(),
            family: FormatFamily::Image,
            name: "PNG Image".to_string(),
            is_container: false,
        }
    }

    /// Create a new JPEG format instance
    #[must_use]
    pub fn jpeg() -> Self {
        Self {
            mime_type: "image/jpeg".to_string(),
            extension: "jpg".to_string(),
            family: FormatFamily::Image,
            name: "JPEG Image".to_string(),
            is_container: false,
        }
    }
}

/// Format families for categorization
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FormatFamily {
    /// PDF documents
    Document,
    /// Microsoft Office and similar
    Office,
    /// Email formats (MSG, EML, PST)
    Email,
    /// Image formats
    Image,
    /// Archive formats (ZIP, RAR, etc.)
    Archive,
    /// CAD formats (DWG, DXF, etc.)
    Cad,
    /// Text and code files
    Text,
    /// Audio files
    Audio,
    /// Video files
    Video,
    /// Legacy/specialty formats
    Legacy,
    /// Unknown/other
    Unknown,
}

impl FormatFamily {
    /// Get a human-readable name for this family
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            FormatFamily::Document => "Document",
            FormatFamily::Office => "Office",
            FormatFamily::Email => "Email",
            FormatFamily::Image => "Image",
            FormatFamily::Archive => "Archive",
            FormatFamily::Cad => "CAD",
            FormatFamily::Text => "Text",
            FormatFamily::Audio => "Audio",
            FormatFamily::Video => "Video",
            FormatFamily::Legacy => "Legacy",
            FormatFamily::Unknown => "Unknown",
        }
    }
}

/// A file format signature (magic bytes)
#[derive(Debug, Clone)]
pub struct FormatSignature {
    /// Bytes to match
    pub bytes: &'static [u8],

    /// Offset from start of file
    pub offset: usize,

    /// Associated format
    pub format: fn() -> Format,
}

/// Detection result with confidence score
#[derive(Debug, Clone)]
pub struct DetectionResult {
    /// Detected format
    pub format: Format,

    /// Confidence score (0.0 - 1.0)
    pub confidence: f64,

    /// How the format was detected
    pub method: DetectionMethod,
}

/// How the format was detected
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionMethod {
    /// Detected via magic bytes
    MagicBytes,
    /// Detected via file extension
    Extension,
    /// Detected via content analysis
    ContentAnalysis,
    /// Detected via container inspection (e.g., ZIP containing Office files)
    ContainerInspection,
}

// =========================================
// Format signatures database
// =========================================

/// Known format signatures
static SIGNATURES: &[FormatSignature] = &[
    // PDF
    FormatSignature {
        bytes: b"%PDF",
        offset: 0,
        format: Format::pdf,
    },
    // PNG
    FormatSignature {
        bytes: &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
        offset: 0,
        format: Format::png,
    },
    // JPEG
    FormatSignature {
        bytes: &[0xFF, 0xD8, 0xFF],
        offset: 0,
        format: Format::jpeg,
    },
    // ZIP (and OOXML which uses ZIP container)
    FormatSignature {
        bytes: &[0x50, 0x4B, 0x03, 0x04],
        offset: 0,
        format: || Format {
            mime_type: "application/zip".to_string(),
            extension: "zip".to_string(),
            family: FormatFamily::Archive,
            name: "ZIP Archive".to_string(),
            is_container: true,
        },
    },
    // GIF
    FormatSignature {
        bytes: b"GIF87a",
        offset: 0,
        format: || Format {
            mime_type: "image/gif".to_string(),
            extension: "gif".to_string(),
            family: FormatFamily::Image,
            name: "GIF Image".to_string(),
            is_container: false,
        },
    },
    FormatSignature {
        bytes: b"GIF89a",
        offset: 0,
        format: || Format {
            mime_type: "image/gif".to_string(),
            extension: "gif".to_string(),
            family: FormatFamily::Image,
            name: "GIF Image".to_string(),
            is_container: false,
        },
    },
    // TIFF (little-endian)
    FormatSignature {
        bytes: &[0x49, 0x49, 0x2A, 0x00],
        offset: 0,
        format: || Format {
            mime_type: "image/tiff".to_string(),
            extension: "tiff".to_string(),
            family: FormatFamily::Image,
            name: "TIFF Image".to_string(),
            is_container: false,
        },
    },
    // TIFF (big-endian)
    FormatSignature {
        bytes: &[0x4D, 0x4D, 0x00, 0x2A],
        offset: 0,
        format: || Format {
            mime_type: "image/tiff".to_string(),
            extension: "tiff".to_string(),
            family: FormatFamily::Image,
            name: "TIFF Image".to_string(),
            is_container: false,
        },
    },
    // OLE Compound File (DOC, XLS, PPT, MSG)
    FormatSignature {
        bytes: &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1],
        offset: 0,
        format: || Format {
            mime_type: "application/x-cfb".to_string(),
            extension: "".to_string(),
            family: FormatFamily::Office,
            name: "OLE Compound File".to_string(),
            is_container: true,
        },
    },
    // RAR
    FormatSignature {
        bytes: &[0x52, 0x61, 0x72, 0x21, 0x1A, 0x07],
        offset: 0,
        format: || Format {
            mime_type: "application/vnd.rar".to_string(),
            extension: "rar".to_string(),
            family: FormatFamily::Archive,
            name: "RAR Archive".to_string(),
            is_container: true,
        },
    },
    // 7z
    FormatSignature {
        bytes: &[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C],
        offset: 0,
        format: || Format {
            mime_type: "application/x-7z-compressed".to_string(),
            extension: "7z".to_string(),
            family: FormatFamily::Archive,
            name: "7-Zip Archive".to_string(),
            is_container: true,
        },
    },
];

/// Extension to format mapping
static EXTENSION_MAP: &[(&str, fn() -> Format)] = &[
    ("pdf", Format::pdf),
    ("docx", Format::docx),
    ("xlsx", Format::xlsx),
    ("pptx", Format::pptx),
    ("png", Format::png),
    ("jpg", Format::jpeg),
    ("jpeg", Format::jpeg),
    // Add more as needed...
];

/// Detect the format of a document from its content
///
/// # Arguments
///
/// * `data` - The document content (at least first 8KB recommended)
/// * `filename` - Optional filename hint for extension-based detection
///
/// # Returns
///
/// The detected format with confidence, or None if unknown
#[must_use]
pub fn detect_format(data: &[u8], filename: Option<&str>) -> Option<DetectionResult> {
    // Try magic bytes first (highest confidence)
    if let Some(result) = detect_by_magic(data) {
        // If it's a ZIP, check if it's actually an Office document
        if result.format.mime_type == "application/zip" {
            if let Some(office_format) = detect_office_in_zip(data) {
                return Some(DetectionResult {
                    format: office_format,
                    confidence: 0.95,
                    method: DetectionMethod::ContainerInspection,
                });
            }
        }
        return Some(result);
    }

    // Try extension-based detection
    if let Some(filename) = filename {
        if let Some(result) = detect_by_extension(filename) {
            return Some(result);
        }
    }

    None
}

/// Detect format by magic bytes
fn detect_by_magic(data: &[u8]) -> Option<DetectionResult> {
    for sig in SIGNATURES {
        if data.len() >= sig.offset + sig.bytes.len() {
            let slice = &data[sig.offset..sig.offset + sig.bytes.len()];
            if slice == sig.bytes {
                return Some(DetectionResult {
                    format: (sig.format)(),
                    confidence: 0.99,
                    method: DetectionMethod::MagicBytes,
                });
            }
        }
    }
    None
}

/// Detect format by file extension
fn detect_by_extension(filename: &str) -> Option<DetectionResult> {
    let ext = filename
        .rsplit('.')
        .next()?
        .to_lowercase();

    for (extension, format_fn) in EXTENSION_MAP {
        if ext == *extension {
            return Some(DetectionResult {
                format: format_fn(),
                confidence: 0.7,
                method: DetectionMethod::Extension,
            });
        }
    }

    None
}

/// Check if a ZIP file is actually an Office document
fn detect_office_in_zip(data: &[u8]) -> Option<Format> {
    // Simple check: look for "[Content_Types].xml" which is present in OOXML
    // In a real implementation, you'd actually parse the ZIP
    
    let content_types = b"[Content_Types].xml";
    if data.windows(content_types.len()).any(|w| w == content_types) {
        // Check for specific document types
        if data.windows(4).any(|w| w == b"word") {
            return Some(Format::docx());
        }
        if data.windows(2).any(|w| w == b"xl") {
            return Some(Format::xlsx());
        }
        if data.windows(3).any(|w| w == b"ppt") {
            return Some(Format::pptx());
        }
    }

    None
}

/// Get format information by MIME type
#[must_use]
pub fn format_by_mime(mime_type: &str) -> Option<Format> {
    match mime_type {
        "application/pdf" => Some(Format::pdf()),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            Some(Format::docx())
        }
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
            Some(Format::xlsx())
        }
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
            Some(Format::pptx())
        }
        "image/png" => Some(Format::png()),
        "image/jpeg" => Some(Format::jpeg()),
        _ => None,
    }
}

/// Get format information by extension
#[must_use]
pub fn format_by_extension(extension: &str) -> Option<Format> {
    let ext = extension.trim_start_matches('.').to_lowercase();
    
    for (e, format_fn) in EXTENSION_MAP {
        if ext == *e {
            return Some(format_fn());
        }
    }
    
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_pdf() {
        let data = b"%PDF-1.4 test content";
        let result = detect_format(data, None);
        
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.format.mime_type, "application/pdf");
        assert_eq!(result.confidence, 0.99);
        assert_eq!(result.method, DetectionMethod::MagicBytes);
    }

    #[test]
    fn test_detect_png() {
        let data = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00];
        let result = detect_format(&data, None);
        
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.format.mime_type, "image/png");
    }

    #[test]
    fn test_detect_by_extension() {
        let result = detect_format(b"unknown content", Some("document.pdf"));
        
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.format.mime_type, "application/pdf");
        assert_eq!(result.method, DetectionMethod::Extension);
        assert!(result.confidence < 0.99); // Lower confidence for extension-based
    }

    #[test]
    fn test_unknown_format() {
        let result = detect_format(b"random bytes", None);
        assert!(result.is_none());
    }

    #[test]
    fn test_format_family() {
        assert_eq!(FormatFamily::Document.name(), "Document");
        assert_eq!(FormatFamily::Office.name(), "Office");
    }
}

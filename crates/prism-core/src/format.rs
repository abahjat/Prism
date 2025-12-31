// SPDX-License-Identifier: AGPL-3.0-only
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
//! // PDF file signature
//! let data = b"%PDF-1.4 test content";
//! let result = detect_format(data, Some("sample.pdf"));
//!
//! assert!(result.is_some());
//! let result = result.unwrap();
//! assert_eq!(result.format.mime_type, "application/pdf");
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
            mime_type: "application/vnd.openxmlformats-officedocument.presentationml.presentation"
                .to_string(),
            extension: "pptx".to_string(),
            family: FormatFamily::Office,
            name: "Microsoft PowerPoint (PPTX)".to_string(),
            is_container: true,
        }
    }

    /// Create a new ODT (`OpenDocument` Text) format instance
    #[must_use]
    pub fn odt() -> Self {
        Self {
            mime_type: "application/vnd.oasis.opendocument.text".to_string(),
            extension: "odt".to_string(),
            family: FormatFamily::Office,
            name: "OpenDocument Text (ODT)".to_string(),
            is_container: true,
        }
    }

    /// Create a new ODS (`OpenDocument` Spreadsheet) format instance
    #[must_use]
    pub fn ods() -> Self {
        Self {
            mime_type: "application/vnd.oasis.opendocument.spreadsheet".to_string(),
            extension: "ods".to_string(),
            family: FormatFamily::Office,
            name: "OpenDocument Spreadsheet (ODS)".to_string(),
            is_container: true,
        }
    }

    /// Create a new ODP (`OpenDocument` Presentation) format instance
    #[must_use]
    pub fn odp() -> Self {
        Self {
            mime_type: "application/vnd.oasis.opendocument.presentation".to_string(),
            extension: "odp".to_string(),
            family: FormatFamily::Office,
            name: "OpenDocument Presentation (ODP)".to_string(),
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

    /// Create a new TIFF format instance
    #[must_use]
    pub fn tiff() -> Self {
        Self {
            mime_type: "image/tiff".to_string(),
            extension: "tif".to_string(),
            family: FormatFamily::Image,
            name: "TIFF Image".to_string(),
            is_container: false,
        }
    }

    /// Create a new GIF format instance
    #[must_use]
    pub fn gif() -> Self {
        Self {
            mime_type: "image/gif".to_string(),
            extension: "gif".to_string(),
            family: FormatFamily::Image,
            name: "GIF Image".to_string(),
            is_container: false,
        }
    }

    /// Create a new WebP format instance
    #[must_use]
    pub fn webp() -> Self {
        Self {
            mime_type: "image/webp".to_string(),
            extension: "webp".to_string(),
            family: FormatFamily::Image,
            name: "WebP Image".to_string(),
            is_container: false,
        }
    }

    /// Create a new BMP format instance
    #[must_use]
    pub fn bmp() -> Self {
        Self {
            mime_type: "image/bmp".to_string(),
            extension: "bmp".to_string(),
            family: FormatFamily::Image,
            name: "BMP Image".to_string(),
            is_container: false,
        }
    }

    /// Create a new SVG format instance
    #[must_use]
    pub fn svg() -> Self {
        Self {
            mime_type: "image/svg+xml".to_string(),
            extension: "svg".to_string(),
            family: FormatFamily::Image,
            name: "SVG Image".to_string(),
            is_container: false,
        }
    }

    /// Create a new EPS (Encapsulated PostScript) format instance
    #[must_use]
    pub fn eps() -> Self {
        Self {
            mime_type: "application/postscript".to_string(),
            extension: "eps".to_string(),
            family: FormatFamily::Image,
            name: "EPS Image".to_string(),
            is_container: false,
        }
    }

    /// Create a new EMF (Enhanced Metafile) format instance
    #[must_use]
    pub fn emf() -> Self {
        Self {
            mime_type: "image/emf".to_string(),
            extension: "emf".to_string(),
            family: FormatFamily::Image,
            name: "EMF Image".to_string(),
            is_container: false,
        }
    }

    /// Create a new EMZ (Compressed Enhanced Metafile) format instance
    #[must_use]
    pub fn emz() -> Self {
        Self {
            mime_type: "application/x-emz".to_string(),
            extension: "emz".to_string(),
            family: FormatFamily::Image,
            name: "EMZ Image".to_string(),
            is_container: false,
        }
    }

    /// Create a new WMF (Windows Metafile) format instance
    #[must_use]
    pub fn wmf() -> Self {
        Self {
            mime_type: "image/wmf".to_string(),
            extension: "wmf".to_string(),
            family: FormatFamily::Image,
            name: "WMF Image".to_string(),
            is_container: false,
        }
    }

    /// Create a new ICO (Windows Icon) format instance
    #[must_use]
    pub fn ico() -> Self {
        Self {
            mime_type: "image/x-icon".to_string(),
            extension: "ico".to_string(),
            family: FormatFamily::Image,
            name: "ICO Image".to_string(),
            is_container: false,
        }
    }

    /// Create a new TGA (Truevision) format instance
    #[must_use]
    pub fn tga() -> Self {
        Self {
            mime_type: "image/x-tga".to_string(),
            extension: "tga".to_string(),
            family: FormatFamily::Image,
            name: "TGA Image".to_string(),
            is_container: false,
        }
    }

    /// Create a new SVGZ (Compressed SVG) format instance
    #[must_use]
    pub fn svgz() -> Self {
        Self {
            mime_type: "image/svg+xml".to_string(),
            extension: "svgz".to_string(),
            family: FormatFamily::Image,
            name: "Compressed SVG Image".to_string(),
            is_container: false,
        }
    }

    /// Create a new ODG (`OpenDocument` Graphics) format instance
    #[must_use]
    pub fn odg() -> Self {
        Self {
            mime_type: "application/vnd.oasis.opendocument.graphics".to_string(),
            extension: "odg".to_string(),
            family: FormatFamily::Office,
            name: "OpenDocument Graphics (ODG)".to_string(),
            is_container: true,
        }
    }

    /// Create a new Microsoft `OneNote` format instance
    #[must_use]
    pub fn onenote() -> Self {
        Self {
            mime_type: "application/onenote".to_string(),
            extension: "one".to_string(),
            family: FormatFamily::Office,
            name: "Microsoft OneNote".to_string(),
            is_container: true,
        }
    }

    /// Create a new Microsoft Visio format instance
    #[must_use]
    pub fn vsdx() -> Self {
        Self {
            mime_type: "application/vnd.ms-visio.drawing.main+xml".to_string(),
            extension: "vsdx".to_string(),
            family: FormatFamily::Office,
            name: "Microsoft Visio".to_string(),
            is_container: true,
        }
    }

    /// Create a new Microsoft Project format instance
    #[must_use]
    pub fn mpp() -> Self {
        Self {
            mime_type: "application/vnd.ms-project".to_string(),
            extension: "mpp".to_string(),
            family: FormatFamily::Office,
            name: "Microsoft Project".to_string(),
            is_container: true,
        }
    }

    /// Create a new plain text format instance
    #[must_use]
    pub fn text() -> Self {
        Self {
            mime_type: "text/plain".to_string(),
            extension: "txt".to_string(),
            family: FormatFamily::Text,
            name: "Plain Text".to_string(),
            is_container: false,
        }
    }

    /// Create a new JSON format instance
    #[must_use]
    pub fn json() -> Self {
        Self {
            mime_type: "application/json".to_string(),
            extension: "json".to_string(),
            family: FormatFamily::Text,
            name: "JSON".to_string(),
            is_container: false,
        }
    }

    /// Create a new XML format instance
    #[must_use]
    pub fn xml() -> Self {
        Self {
            mime_type: "application/xml".to_string(),
            extension: "xml".to_string(),
            family: FormatFamily::Text,
            name: "XML".to_string(),
            is_container: false,
        }
    }

    /// Create a new CSV format instance
    #[must_use]
    pub fn csv() -> Self {
        Self {
            mime_type: "text/csv".to_string(),
            extension: "csv".to_string(),
            family: FormatFamily::Text,
            name: "CSV".to_string(),
            is_container: false,
        }
    }

    /// Create a new Markdown format instance
    #[must_use]
    pub fn markdown() -> Self {
        Self {
            mime_type: "text/markdown".to_string(),
            extension: "md".to_string(),
            family: FormatFamily::Text,
            name: "Markdown".to_string(),
            is_container: false,
        }
    }

    /// Create a new log file format instance
    #[must_use]
    pub fn log() -> Self {
        Self {
            mime_type: "text/plain".to_string(),
            extension: "log".to_string(),
            family: FormatFamily::Text,
            name: "Log File".to_string(),
            is_container: false,
        }
    }

    /// Create a new HTML format instance
    #[must_use]
    pub fn html() -> Self {
        Self {
            mime_type: "text/html".to_string(),
            extension: "html".to_string(),
            family: FormatFamily::Text,
            name: "HTML".to_string(),
            is_container: false,
        }
    }

    /// Create a new MHT format instance (MIME HTML)
    #[must_use]
    pub fn mht() -> Self {
        Self {
            mime_type: "multipart/related".to_string(),
            extension: "mht".to_string(),
            family: FormatFamily::Archive, // Acts as a web archive
            name: "MIME HTML Archive".to_string(),
            is_container: true,
        }
    }

    /// Create a new EPUB format instance
    #[must_use]
    pub fn epub() -> Self {
        Self {
            mime_type: "application/epub+zip".to_string(),
            extension: "epub".to_string(),
            family: FormatFamily::Document,
            name: "EPUB E-Book".to_string(),
            is_container: true,
        }
    }

    /// Create a new XPS format instance
    #[must_use]
    pub fn xps() -> Self {
        Self {
            mime_type: "application/vnd.ms-xpsdocument".to_string(),
            extension: "xps".to_string(),
            family: FormatFamily::Document,
            name: "XPS Document".to_string(),
            is_container: true,
        }
    }

    /// Create a new RTF format instance (Rich Text Format)
    #[must_use]
    pub fn rtf() -> Self {
        Self {
            mime_type: "application/rtf".to_string(),
            extension: "rtf".to_string(),
            family: FormatFamily::Document,
            name: "Rich Text Format (RTF)".to_string(),
            is_container: false,
        }
    }

    /// Create a new DOC format instance (Word 97-2003)
    #[must_use]
    pub fn doc() -> Self {
        Self {
            mime_type: "application/msword".to_string(),
            extension: "doc".to_string(),
            family: FormatFamily::Office,
            name: "Microsoft Word 97-2003".to_string(),
            is_container: true,
        }
    }

    /// Create a new XLS format instance (Excel 97-2003)
    #[must_use]
    pub fn xls() -> Self {
        Self {
            mime_type: "application/vnd.ms-excel".to_string(),
            extension: "xls".to_string(),
            family: FormatFamily::Office,
            name: "Microsoft Excel 97-2003".to_string(),
            is_container: true,
        }
    }

    /// Create a new PPT format instance (`PowerPoint` 97-2003)
    #[must_use]
    pub fn ppt() -> Self {
        Self {
            mime_type: "application/vnd.ms-powerpoint".to_string(),
            extension: "ppt".to_string(),
            family: FormatFamily::Office,
            name: "Microsoft PowerPoint 97-2003".to_string(),
            is_container: true,
        }
    }

    /// Create a new EML format instance (Email Message)
    #[must_use]
    pub fn eml() -> Self {
        Self {
            mime_type: "message/rfc822".to_string(),
            extension: "eml".to_string(),
            family: FormatFamily::Email,
            name: "Email Message".to_string(),
            is_container: false,
        }
    }

    /// Create a new MSG format instance (Outlook Message)
    #[must_use]
    pub fn msg() -> Self {
        Self {
            mime_type: "application/vnd.ms-outlook".to_string(),
            extension: "msg".to_string(),
            family: FormatFamily::Email,
            name: "Outlook Message".to_string(),
            is_container: false,
        }
    }

    /// Create a new MBOX format instance (Email Mailbox)
    #[must_use]
    pub fn mbox() -> Self {
        Self {
            mime_type: "application/mbox".to_string(),
            extension: "mbox".to_string(),
            family: FormatFamily::Email,
            name: "Email Mailbox".to_string(),
            is_container: true,
        }
    }

    /// Create a new VCF format instance (vCard Contact)
    #[must_use]
    pub fn vcf() -> Self {
        Self {
            mime_type: "text/vcard".to_string(),
            extension: "vcf".to_string(),
            family: FormatFamily::Contact,
            name: "vCard Contact".to_string(),
            is_container: false,
        }
    }

    /// Create a new ICS format instance (iCalendar)
    #[must_use]
    pub fn ics() -> Self {
        Self {
            mime_type: "text/calendar".to_string(),
            extension: "ics".to_string(),
            family: FormatFamily::Email,
            name: "iCalendar".to_string(),
            is_container: false,
        }
    }
    /// Create a new ZIP format instance
    #[must_use]
    pub fn zip() -> Self {
        Self {
            mime_type: "application/zip".to_string(),
            extension: "zip".to_string(),
            family: FormatFamily::Archive,
            name: "ZIP Archive".to_string(),
            is_container: true,
        }
    }

    /// Create a new TAR format instance
    #[must_use]
    pub fn tar() -> Self {
        Self {
            mime_type: "application/x-tar".to_string(),
            extension: "tar".to_string(),
            family: FormatFamily::Archive,
            name: "TAR Archive".to_string(),
            is_container: true,
        }
    }

    /// Create a new GZIP format instance
    #[must_use]
    pub fn gzip() -> Self {
        Self {
            mime_type: "application/gzip".to_string(),
            extension: "gz".to_string(),
            family: FormatFamily::Archive,
            name: "GZIP Compressed File".to_string(),
            is_container: false, // It's a compressor, but effectively behaves like single-file container
        }
    }

    /// Create a new DXF format instance (`AutoCAD` Drawing Exchange Format)
    #[must_use]
    pub fn dxf() -> Self {
        Self {
            mime_type: "image/vnd.dxf".to_string(),
            extension: "dxf".to_string(),
            family: FormatFamily::Cad,
            name: "AutoCAD DXF".to_string(),
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
    /// Contact formats (VCF, vCard)
    Contact,
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
            FormatFamily::Contact => "Contact",
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
        format: Format::zip,
    },
    // TAR (USTAR)
    FormatSignature {
        bytes: &[0x75, 0x73, 0x74, 0x61, 0x72], // "ustar"
        offset: 257,
        format: Format::tar,
    },
    // GZIP
    FormatSignature {
        bytes: &[0x1F, 0x8B],
        offset: 0,
        format: Format::gzip,
    },
    // GIF
    FormatSignature {
        bytes: b"GIF87a",
        offset: 0,
        format: Format::gif,
    },
    FormatSignature {
        bytes: b"GIF89a",
        offset: 0,
        format: Format::gif,
    },
    // WebP (RIFF....WEBP format)
    FormatSignature {
        bytes: b"RIFF",
        offset: 0,
        format: Format::webp, // Note: Also needs WEBP at offset 8, handled in can_parse
    },
    // TIFF (little-endian)
    FormatSignature {
        bytes: &[0x49, 0x49, 0x2A, 0x00],
        offset: 0,
        format: Format::tiff,
    },
    // TIFF (big-endian)
    FormatSignature {
        bytes: &[0x4D, 0x4D, 0x00, 0x2A],
        offset: 0,
        format: Format::tiff,
    },
    // OLE Compound File (DOC, XLS, PPT, MSG)
    FormatSignature {
        bytes: &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1],
        offset: 0,
        format: || Format {
            mime_type: "application/x-cfb".to_string(),
            extension: String::new(),
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
    // RTF (Rich Text Format)
    FormatSignature {
        bytes: b"{\\rtf",
        offset: 0,
        format: Format::rtf,
    },
    // =========================================
    // Legacy Office formats (pre-OLE2)
    // =========================================
    // Word 2.0 for Windows
    FormatSignature {
        bytes: &[0xDB, 0xA5, 0x2D, 0x00],
        offset: 0,
        format: Format::doc,
    },
    // Word for Mac 1.0 / Write for Atari ST
    FormatSignature {
        bytes: &[0xFE, 0x32, 0x00],
        offset: 0,
        format: Format::doc,
    },
    // Word for Mac 3.0
    FormatSignature {
        bytes: &[0xFE, 0x34, 0x00],
        offset: 0,
        format: Format::doc,
    },
    // Word for Mac 4.0
    FormatSignature {
        bytes: &[0xFE, 0x37, 0x00, 0x1C],
        offset: 0,
        format: Format::doc,
    },
    // Word for Mac 5.0
    FormatSignature {
        bytes: &[0xFE, 0x37, 0x00, 0x23],
        offset: 0,
        format: Format::doc,
    },
    // Windows Write document
    FormatSignature {
        bytes: &[0x31, 0xBE, 0x00, 0x00, 0x00, 0xAB, 0x00],
        offset: 0,
        format: || Format {
            mime_type: "application/x-mswrite".to_string(),
            extension: "wri".to_string(),
            family: FormatFamily::Legacy,
            name: "Windows Write".to_string(),
            is_container: false,
        },
    },
    // Windows Write document with OLE objects
    FormatSignature {
        bytes: &[0x32, 0xBE, 0x00, 0x00, 0x00, 0xAB, 0x00],
        offset: 0,
        format: || Format {
            mime_type: "application/x-mswrite".to_string(),
            extension: "wri".to_string(),
            family: FormatFamily::Legacy,
            name: "Windows Write (OLE)".to_string(),
            is_container: false,
        },
    },
    // PowerPoint 2.0 (pre-OLE2)
    FormatSignature {
        bytes: &[0xED, 0xDE, 0xAD, 0x0B, 0x02, 0x00, 0x00, 0x00],
        offset: 0,
        format: Format::ppt,
    },
    // PowerPoint 3.0 (pre-OLE2)
    FormatSignature {
        bytes: &[0xED, 0xDE, 0xAD, 0x0B, 0x03, 0x00, 0x00, 0x00],
        offset: 0,
        format: Format::ppt,
    },
    // Excel 4.0 worksheet
    FormatSignature {
        bytes: &[0x09, 0x04, 0x06, 0x00, 0x00],
        offset: 0,
        format: Format::xls,
    },
    // Excel for OS/2 (various versions)
    FormatSignature {
        bytes: &[0x09, 0x00, 0x04, 0x00, 0x05, 0x00],
        offset: 0,
        format: Format::xls,
    },
    // Excel generic older format
    FormatSignature {
        bytes: &[0x09, 0x08],
        offset: 0,
        format: Format::xls,
    },
    // WordStar document
    FormatSignature {
        bytes: &[0x1D, 0x7D, 0x00, 0x00],
        offset: 0,
        format: || Format {
            mime_type: "application/x-wordstar".to_string(),
            extension: "ws".to_string(),
            family: FormatFamily::Legacy,
            name: "WordStar Document".to_string(),
            is_container: false,
        },
    },
    // =========================================
    // CAD formats
    // =========================================
    // DXF (Binary)
    FormatSignature {
        bytes: b"AutoCAD Binary DXF",
        offset: 0,
        format: Format::dxf,
    },
];

/// Extension to format mapping
#[allow(clippy::type_complexity)]
static EXTENSION_MAP: &[(&str, fn() -> Format)] = &[
    ("pdf", Format::pdf),
    ("docx", Format::docx),
    ("xlsx", Format::xlsx),
    ("pptx", Format::pptx),
    ("doc", Format::doc),
    ("xls", Format::xls),
    ("ppt", Format::ppt),
    ("odt", Format::odt),
    ("ods", Format::ods),
    ("odp", Format::odp),
    ("png", Format::png),
    ("jpg", Format::jpeg),
    ("jpeg", Format::jpeg),
    ("tif", Format::tiff),
    ("tiff", Format::tiff),
    ("gif", Format::gif),
    ("webp", Format::webp),
    ("bmp", Format::bmp),
    ("svg", Format::svg),
    ("eps", Format::eps),
    ("emf", Format::emf),
    ("emz", Format::emz),
    ("wmf", Format::wmf),
    ("ico", Format::ico),
    ("tga", Format::tga),
    ("svgz", Format::svgz),
    ("odg", Format::odg),
    ("one", Format::onenote),
    ("vsdx", Format::vsdx),
    ("mpp", Format::mpp),
    ("txt", Format::text),
    ("json", Format::json),
    ("xml", Format::xml),
    ("csv", Format::csv),
    ("md", Format::markdown),
    ("log", Format::log),
    ("html", Format::html),
    ("htm", Format::html),
    ("eml", Format::eml),
    ("msg", Format::msg),
    ("mbox", Format::mbox),
    ("mht", Format::mht),
    ("mhtml", Format::mht),
    ("epub", Format::epub),
    ("xps", Format::xps),
    ("oxps", Format::xps),
    ("vcf", Format::vcf),
    ("vcard", Format::vcf),
    ("ics", Format::ics),
    ("zip", Format::zip),
    ("tar", Format::tar),
    ("gz", Format::gzip),
    ("gzip", Format::gzip),
    ("tgz", Format::gzip), // Often treated as gzip then tar
    ("rtf", Format::rtf),
    ("wri", || Format {
        mime_type: "application/x-mswrite".to_string(),
        extension: "wri".to_string(),
        family: FormatFamily::Legacy,
        name: "Windows Write".to_string(),
        is_container: false,
    }),
    ("ws", || Format {
        mime_type: "application/x-wordstar".to_string(),
        extension: "ws".to_string(),
        family: FormatFamily::Legacy,
        name: "WordStar Document".to_string(),
        is_container: false,
    }),
    // CAD formats
    ("dxf", Format::dxf),
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
        // If it's OLE2/CFB, check if it's a legacy Office document
        if result.format.mime_type == "application/x-cfb" {
            if let Some(office_format) = detect_office_in_ole(data, filename) {
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
    let ext = filename.rsplit('.').next()?.to_lowercase();

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
    // Check for ODF formats first - they have a "mimetype" file at the start
    // ODF mimetype file is typically uncompressed and at the beginning of the ZIP
    let mimetype_marker = b"mimetypeapplication/vnd.oasis.opendocument.";
    if let Some(pos) = data
        .windows(mimetype_marker.len())
        .position(|w| w == mimetype_marker)
    {
        // Check what type of ODF document
        let after_marker = &data[pos + mimetype_marker.len()..];
        if after_marker.starts_with(b"text") {
            return Some(Format::odt());
        }
        if after_marker.starts_with(b"spreadsheet") {
            return Some(Format::ods());
        }
        if after_marker.starts_with(b"presentation") {
            return Some(Format::odp());
        }
        if after_marker.starts_with(b"graphics") {
            return Some(Format::odg());
        }
    }

    // Check for EPUB
    let epub_marker = b"mimetypeapplication/epub+zip";
    if data.windows(epub_marker.len()).any(|w| w == epub_marker) {
        return Some(Format::epub());
    }

    // Check for XPS
    if data
        .windows(27)
        .any(|w| w == b"FixedDocumentSequence.fdseq")
    {
        return Some(Format::xps());
    }

    // Simple check: look for "[Content_Types].xml" which is present in OOXML
    // In a real implementation, you'd actually parse the ZIP

    let content_types = b"[Content_Types].xml";
    if data
        .windows(content_types.len())
        .any(|w| w == content_types)
    {
        // Check for specific document types - order matters, check most specific first
        // Look for directory names which are more reliable
        if data.windows(9).any(|w| w == b"ppt/slides") || data.windows(3).any(|w| w == b"ppt") {
            return Some(Format::pptx());
        }
        if data.windows(9).any(|w| w == b"xl/workbook")
            || data.windows(13).any(|w| w == b"xl/worksheets")
        {
            return Some(Format::xlsx());
        }
        if data.windows(10).any(|w| w == b"word/document") || data.windows(4).any(|w| w == b"word")
        {
            return Some(Format::docx());
        }
    }

    None
}

/// Detect specific Office format in OLE2/CFB files (DOC, XLS, PPT, MSG)
/// Note: This function should only be called if magic bytes already confirmed OLE2/CFB format
fn detect_office_in_ole(data: &[u8], filename: Option<&str>) -> Option<Format> {
    // Look for stream names in the OLE2 structure
    // Note: CFB Directory Entry names are UTF-16LE encoded

    // MSG detection - Prioritize this as it has very specific signatures
    // "__properties_version1.0" in UTF-16LE
    let prop_ver_utf16: &[u8] = &[
        0x5F, 0x00, 0x5F, 0x00, 0x70, 0x00, 0x72, 0x00, 0x6F, 0x00, 0x70, 0x00, 0x65, 0x00, 0x72,
        0x00, 0x74, 0x00, 0x69, 0x00, 0x65, 0x00, 0x73, 0x00, 0x5F, 0x00, 0x76, 0x00, 0x65, 0x00,
        0x72, 0x00, 0x73, 0x00, 0x69, 0x00, 0x6F, 0x00, 0x6E, 0x00, 0x31, 0x00, 0x2E, 0x00, 0x30,
        0x00,
    ];

    // "__substg1.0_" in UTF-16LE
    let substg_utf16: &[u8] = &[
        0x5F, 0x00, 0x5F, 0x00, 0x73, 0x00, 0x75, 0x00, 0x62, 0x00, 0x73, 0x00, 0x74, 0x00, 0x67,
        0x00, 0x31, 0x00, 0x2E, 0x00, 0x30, 0x00, 0x5F, 0x00,
    ];

    if data.windows(prop_ver_utf16.len()).any(|w| w == prop_ver_utf16)
        || data.windows(substg_utf16.len()).any(|w| w == substg_utf16)
        // Keep ASCII check just in case of weird implementations or non-OLE containers reusing this logic
        || data.windows(23).any(|w| w == b"__properties_version1.0")
        || data.windows(12).any(|w| w == b"__substg1.0_")
    {
        return Some(Format::msg());
    }

    // fallback MSG check by extension
    if let Some(filename) = filename {
        let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
        if ext == "msg" {
            return Some(Format::msg());
        }
    }

    // Word: "WordDocument" in UTF-16LE
    let word_doc_utf16 = b"W\0o\0r\0d\0D\0o\0c\0u\0m\0e\0n\0t";
    if data
        .windows(word_doc_utf16.len())
        .any(|w| w == word_doc_utf16)
        || data.windows(12).any(|w| w == b"WordDocument")
    {
        return Some(Format::doc());
    }

    // Excel: "Workbook" or "Book"
    let workbook_utf16 = b"W\0o\0r\0k\0b\0o\0o\0k";
    let book_utf16 = b"B\0o\0o\0k";
    if data
        .windows(workbook_utf16.len())
        .any(|w| w == workbook_utf16)
        || data.windows(book_utf16.len()).any(|w| w == book_utf16)
        || data.windows(8).any(|w| w == b"Workbook")
    {
        return Some(Format::xls());
    }

    // PowerPoint: "PowerPoint Document" or "Current User"
    let ppt_doc_utf16 = b"P\0o\0w\0e\0r\0P\0o\0i\0n\0t\0 \0D\0o\0c\0u\0m\0e\0n\0t";
    let current_user_utf16 = b"C\0u\0r\0r\0e\0n\0t\0 \0U\0s\0e\0r";
    if data
        .windows(ppt_doc_utf16.len())
        .any(|w| w == ppt_doc_utf16)
        || data
            .windows(current_user_utf16.len())
            .any(|w| w == current_user_utf16)
        || data.windows(19).any(|w| w == b"PowerPoint Document")
        || data.windows(12).any(|w| w == b"Current User")
    {
        return Some(Format::ppt());
    }

    // Project: "MSProject"
    let mpp_utf16 = b"M\0S\0P\0r\0o\0j\0e\0c\0t";
    if data.windows(mpp_utf16.len()).any(|w| w == mpp_utf16)
        || data.windows(9).any(|w| w == b"MSProject")
    {
        return Some(Format::mpp());
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
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => Some(Format::xlsx()),
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
            Some(Format::pptx())
        }
        "image/png" => Some(Format::png()),
        "image/jpeg" => Some(Format::jpeg()),
        "image/tiff" => Some(Format::tiff()),
        "text/html" => Some(Format::html()),
        "multipart/related" => Some(Format::mht()),
        "application/epub+zip" => Some(Format::epub()),
        "application/vnd.ms-xpsdocument" | "application/oxps" => Some(Format::xps()),
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
        assert!((result.confidence - 0.99).abs() < f64::EPSILON);
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

    #[test]
    fn test_detect_msg_by_content() {
        // Mock OLE file with MSG properties
        let mut data = vec![
            0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, // OLE magic
        ];
        // Padding
        data.extend_from_slice(&[0; 100]);
        // MSG property signature (UTF-16 "__properties_version1.0")
        data.extend_from_slice(&[
            0x5F, 0x00, 0x5F, 0x00, 0x70, 0x00, 0x72, 0x00, 0x6F, 0x00, 0x70, 0x00, 0x65, 0x00,
            0x72, 0x00, 0x74, 0x00, 0x69, 0x00, 0x65, 0x00, 0x73, 0x00, 0x5F, 0x00, 0x76, 0x00,
            0x65, 0x00, 0x72, 0x00, 0x73, 0x00, 0x69, 0x00, 0x6F, 0x00, 0x6E, 0x00, 0x31, 0x00,
            0x2E, 0x00, 0x30, 0x00,
        ]);

        // Should detect as MSG even without extension
        let result = detect_format(&data, None);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.format.name, "Outlook Message");
        assert_eq!(result.format.mime_type, "application/vnd.ms-outlook");
    }
}

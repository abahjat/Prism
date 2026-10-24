// SPDX-License-Identifier: AGPL-3.0-only
//! Cross-format hidden-content and forensic review report (`HiddenContentReport`).
//!
//! Surfaces every piece of content in a document that a reader of the default "as printed"
//! view might not notice: tracked revisions, comments, concealed text (`vanish`, white/low-contrast
//! text, tiny `< 2 pt` text, off-page/off-slide text, covered text, `;;;` cells, zero-width
//! characters), hidden structure (hidden slides, hidden/veryHidden worksheets, hidden rows/columns,
//! collapsed outline groups, hidden defined names, hidden PDF layers/OCGs, speaker notes),
//! active/linked content (macros, `ActiveX`, OLE objects, external links, dynamic fields, PDF JS,
//! launch actions, embedded files, remote email images), and metadata of forensic interest.

#![allow(
    clippy::doc_markdown,
    clippy::needless_pass_by_value,
    clippy::too_many_lines
)]

use serde::{Deserialize, Serialize};

/// Maximum UTF-8 character count stored in [`Finding::excerpt`].
pub const MAX_EXCERPT_CHARS: usize = 240;

/// Truncate a string to at most [`MAX_EXCERPT_CHARS`] characters for [`Finding::excerpt`].
#[must_use]
pub fn bounded_excerpt(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= MAX_EXCERPT_CHARS {
        trimmed.to_string()
    } else {
        let mut s: String = trimmed.chars().take(MAX_EXCERPT_CHARS).collect();
        s.push('…');
        s
    }
}

/// Back-reference to a UDM / layout page, block, and run.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct SourceRef {
    /// Zero-based page index.
    pub page: u32,
    /// Zero-based block index on the page.
    pub block: u32,
    /// Zero-based run index within the block.
    pub run: u32,
}

impl SourceRef {
    /// Construct a new [`SourceRef`].
    #[must_use]
    pub const fn new(page: u32, block: u32, run: u32) -> Self {
        Self { page, block, run }
    }
}

/// Axis-aligned bounding rectangle in points (`1 pt = 1/72 in`, origin top-left, `+y` down)
/// used in [`Location`] and review overlays.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ReviewRect {
    /// Left X coordinate in points.
    pub x: f32,
    /// Top Y coordinate in points.
    pub y: f32,
    /// Width in points.
    pub width: f32,
    /// Height in points.
    pub height: f32,
}

impl ReviewRect {
    /// Construct a new [`ReviewRect`].
    #[must_use]
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// Cross-format hidden-content and forensic findings report for a document.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HiddenContentReport {
    /// Discovered findings in deterministic document order.
    pub items: Vec<Finding>,
    /// Per-format coverage declaration so "no findings" is never mistaken for "not checked".
    pub coverage: Vec<FormatCoverage>,
}

impl HiddenContentReport {
    /// Create a new [`HiddenContentReport`] with coverage initialized for `format_id`.
    #[must_use]
    pub fn for_format(format_id: &str) -> Self {
        Self {
            items: Vec::new(),
            coverage: vec![FormatCoverage::for_format(format_id)],
        }
    }

    /// Append a [`Finding`] to this report.
    pub fn push(&mut self, finding: Finding) {
        self.items.push(finding);
    }

    /// Return findings matching `kind`.
    #[must_use]
    pub fn by_kind(&self, kind: FindingKind) -> Vec<&Finding> {
        self.items.iter().filter(|f| f.kind == kind).collect()
    }

    /// Return `true` if any finding has [`FindingKind`] `kind`.
    #[must_use]
    pub fn has_kind(&self, kind: FindingKind) -> bool {
        self.items.iter().any(|f| f.kind == kind)
    }
}

/// A single hidden-content or forensic finding in a [`HiddenContentReport`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// Specific category/kind of hidden or forensic content.
    pub kind: FindingKind,
    /// Severity level (`info`, `notable`, `high`).
    pub severity: Severity,
    /// Location within the document (page/slide/sheet + rects, part path, or cell range).
    pub location: Location,
    /// Optional back-reference into the layout page model for direct jump-to in the viewer.
    pub source: Option<SourceRef>,
    /// Bounded text excerpt (at most [`MAX_EXCERPT_CHARS`] characters) when applicable.
    pub excerpt: Option<String>,
    /// Kind-specific structured metadata (author, date, color, font size, target URL, etc.).
    pub detail: serde_json::Value,
}

impl Finding {
    /// Construct a new [`Finding`] with bounded excerpt normalization.
    #[must_use]
    pub fn new(
        kind: FindingKind,
        severity: Severity,
        location: Location,
        source: Option<SourceRef>,
        excerpt: Option<String>,
        detail: serde_json::Value,
    ) -> Self {
        Self {
            kind,
            severity,
            location,
            source,
            excerpt: excerpt.as_deref().map(bounded_excerpt),
            detail,
        }
    }
}

/// Severity of a [`Finding`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Informational finding (e.g., standard author metadata, speaker notes).
    Info,
    /// Notable finding warranting reviewer attention (e.g., tracked changes, comments, hidden rows).
    Notable,
    /// High-priority concealment or active content (e.g., `veryHidden` sheet, white-on-white text, macros, PDF JS).
    High,
}

/// High-level grouping category for [`FindingKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingCategory {
    /// Tracked changes, comments, and markup annotations.
    RevisionsAndComments,
    /// Visually concealed text (hidden attribute, white/low-contrast, tiny, off-page, covered, `;;;`, zero-width).
    ConcealedText,
    /// Structurally hidden containers (hidden slides/sheets/rows/cols, collapsed outlines, hidden names, OCGs, notes).
    HiddenStructure,
    /// Executable, dynamic, or external content (macros, ActiveX, OLE, external links, fields, JS, embedded files).
    ActiveAndLinkedContent,
    /// Forensic metadata and revision history anomalies.
    MetadataOfInterest,
}

/// Specific kind of hidden or forensic content discovered in a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    // --- 1. Revisions and comments ---
    /// Tracked text insertion (`w:ins`).
    TrackedInsertion,
    /// Tracked text deletion (`w:del`).
    TrackedDeletion,
    /// Tracked move (`w:moveFrom` / `w:moveTo`).
    TrackedMove,
    /// Tracked formatting or property change (`w:rPrChange`, `w:pPrChange`, `w:tblPrChange`).
    TrackedFormatChange,
    /// Document, slide, or cell comment / reply.
    Comment,
    /// PDF markup or text annotation (`/Annot`).
    PdfAnnotation,

    // --- 2. Concealed text ---
    /// Explicitly hidden text (`w:vanish`, `w:specVanish`, or hidden character property).
    HiddenText,
    /// White-on-white text or text matching its background/cell fill color.
    WhiteOrLowContrastText,
    /// Extremely small text (`< 2 pt`).
    TinyText,
    /// Text positioned outside the visible page or slide bounds.
    OffPageOrOffSlideText,
    /// Text occluded/covered by an opaque shape or image drawn over it.
    CoveredText,
    /// Spreadsheet cell whose number format suppresses display (`;;;`).
    CustomSuppressedCell,
    /// Text containing zero-width or invisible Unicode format characters (`U+200B`, `U+200C`, `U+200D`, `U+FEFF`).
    ZeroWidthCharacters,

    // --- 3. Hidden structure ---
    /// Hidden presentation slide (`show="0"`).
    HiddenSlide,
    /// Hidden or `veryHidden` spreadsheet worksheet.
    HiddenSheet,
    /// Hidden spreadsheet rows or columns.
    HiddenRowsOrColumns,
    /// Collapsed outline or row/column grouping level.
    CollapsedOutlineGroup,
    /// Hidden workbook defined name (`hidden="1"`).
    HiddenDefinedName,
    /// PDF Optional Content Group (layer) defaulted to `OFF`.
    HiddenPdfLayer,
    /// Presentation speaker notes.
    SpeakerNotes,

    // --- 4. Active and linked content ---
    /// Embedded macro code (VBA project `vbaProject.bin` or Excel 4.0 XLM macro sheet).
    MacroCode,
    /// ActiveX control (`activeX`).
    ActiveXControl,
    /// Embedded OLE object or package.
    EmbeddedOleObject,
    /// External hyperlink, workbook link, external relationship, or data connection.
    ExternalLinkOrConnection,
    /// Dynamic field or formula (`INCLUDETEXT`, `INCLUDEPICTURE`, `DDE`, `DDEAUTO`, unevaluated CF expression).
    DynamicField,
    /// PDF JavaScript action (`/JS` / `/JavaScript`).
    PdfJavaScript,
    /// PDF `/Launch` or `/URI` action.
    PdfLaunchAction,
    /// Embedded file stream or container attachment.
    EmbeddedFile,
    /// Remote image reference (`http://` / `https://`) inside email HTML.
    RemoteEmailImage,

    // --- 5. Metadata of interest ---
    /// Author, last-modified-by, template, or revision count metadata.
    AuthorAndModificationHistory,
    /// Custom document properties or Word document variables (`w:docVars`).
    CustomPropertiesOrVariables,
    /// Word editing session diversity (RSID count).
    RsidDiversity,
    /// XMP metadata packet / history entries.
    XmpHistory,
    /// Incremental PDF updates (`%%EOF` revisions).
    IncrementalPdfUpdates,
    /// Email headers inconsistent with transport (`From` vs `Return-Path` / `Reply-To` / `Received`).
    InconsistentEmailHeaders,
}

impl FindingKind {
    /// Return the high-level [`FindingCategory`] for this finding kind.
    #[must_use]
    pub const fn category(self) -> FindingCategory {
        match self {
            Self::TrackedInsertion
            | Self::TrackedDeletion
            | Self::TrackedMove
            | Self::TrackedFormatChange
            | Self::Comment
            | Self::PdfAnnotation => FindingCategory::RevisionsAndComments,

            Self::HiddenText
            | Self::WhiteOrLowContrastText
            | Self::TinyText
            | Self::OffPageOrOffSlideText
            | Self::CoveredText
            | Self::CustomSuppressedCell
            | Self::ZeroWidthCharacters => FindingCategory::ConcealedText,

            Self::HiddenSlide
            | Self::HiddenSheet
            | Self::HiddenRowsOrColumns
            | Self::CollapsedOutlineGroup
            | Self::HiddenDefinedName
            | Self::HiddenPdfLayer
            | Self::SpeakerNotes => FindingCategory::HiddenStructure,

            Self::MacroCode
            | Self::ActiveXControl
            | Self::EmbeddedOleObject
            | Self::ExternalLinkOrConnection
            | Self::DynamicField
            | Self::PdfJavaScript
            | Self::PdfLaunchAction
            | Self::EmbeddedFile
            | Self::RemoteEmailImage => FindingCategory::ActiveAndLinkedContent,

            Self::AuthorAndModificationHistory
            | Self::CustomPropertiesOrVariables
            | Self::RsidDiversity
            | Self::XmpHistory
            | Self::IncrementalPdfUpdates
            | Self::InconsistentEmailHeaders => FindingCategory::MetadataOfInterest,
        }
    }

    /// Human-readable display label for UI panels and CLI reports.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::TrackedInsertion => "Tracked Insertion",
            Self::TrackedDeletion => "Tracked Deletion",
            Self::TrackedMove => "Tracked Move",
            Self::TrackedFormatChange => "Tracked Formatting Change",
            Self::Comment => "Comment",
            Self::PdfAnnotation => "PDF Annotation",
            Self::HiddenText => "Hidden Text (vanish)",
            Self::WhiteOrLowContrastText => "White or Low-Contrast Text",
            Self::TinyText => "Tiny Text (< 2 pt)",
            Self::OffPageOrOffSlideText => "Off-Page / Off-Slide Text",
            Self::CoveredText => "Covered / Occluded Text",
            Self::CustomSuppressedCell => "Suppressed Cell Format (;;;)",
            Self::ZeroWidthCharacters => "Zero-Width Characters",
            Self::HiddenSlide => "Hidden Slide",
            Self::HiddenSheet => "Hidden Worksheet",
            Self::HiddenRowsOrColumns => "Hidden Rows or Columns",
            Self::CollapsedOutlineGroup => "Collapsed Outline Group",
            Self::HiddenDefinedName => "Hidden Defined Name",
            Self::HiddenPdfLayer => "Hidden PDF Layer (OCG)",
            Self::SpeakerNotes => "Speaker Notes",
            Self::MacroCode => "Macro Code (VBA / XLM)",
            Self::ActiveXControl => "ActiveX Control",
            Self::EmbeddedOleObject => "Embedded OLE Object",
            Self::ExternalLinkOrConnection => "External Link or Connection",
            Self::DynamicField => "Dynamic Field or Formula",
            Self::PdfJavaScript => "PDF JavaScript",
            Self::PdfLaunchAction => "PDF Launch Action",
            Self::EmbeddedFile => "Embedded File / Attachment",
            Self::RemoteEmailImage => "Remote Email Image",
            Self::AuthorAndModificationHistory => "Author & Modification History",
            Self::CustomPropertiesOrVariables => "Custom Properties / Variables",
            Self::RsidDiversity => "Editing Session Diversity (RSIDs)",
            Self::XmpHistory => "XMP Metadata History",
            Self::IncrementalPdfUpdates => "Incremental PDF Updates",
            Self::InconsistentEmailHeaders => "Inconsistent Email Headers",
        }
    }
}

/// Location of a [`Finding`] within a document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Location {
    /// Positioned on a specific 0-based page with optional bounding rectangles in points.
    PageRects {
        /// Zero-based page index.
        page_index: u32,
        /// Bounding rectangles in points (`1 pt = 1/72 in`).
        rects: Vec<ReviewRect>,
    },
    /// Positioned on a specific 0-based presentation slide with optional bounding rectangles.
    Slide {
        /// Zero-based slide index.
        slide_index: u32,
        /// Bounding rectangles in points.
        rects: Vec<ReviewRect>,
    },
    /// Positioned on a spreadsheet worksheet and optional A1 cell or range reference.
    SheetRange {
        /// Worksheet name.
        sheet_name: String,
        /// Zero-based worksheet index.
        sheet_index: u32,
        /// Optional A1 cell or range (e.g. `"B4"`, `"A1:D10"`, `"Row 5"`, `"Col C"`).
        cell_range: Option<String>,
    },
    /// Package part path or archive member path (e.g. `"word/vbaProject.bin"`).
    PartPath {
        /// Path within the package or container.
        path: String,
    },
    /// Document-wide finding (e.g. document metadata, incremental PDF updates).
    Document,
}

impl Location {
    /// Return the 0-based page or slide index if this location targets a specific page/slide.
    #[must_use]
    pub const fn page_index(&self) -> Option<u32> {
        match self {
            Self::PageRects { page_index, .. } => Some(*page_index),
            Self::Slide { slide_index, .. } => Some(*slide_index),
            Self::SheetRange { sheet_index, .. } => Some(*sheet_index),
            Self::PartPath { .. } | Self::Document => None,
        }
    }
}

/// Declaration of which [`FindingKind`]s a format's hidden-content provider inspects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatCoverage {
    /// Canonical format identifier (`"docx"`, `"doc"`, `"pptx"`, `"ppt"`, `"xlsx"`, `"xls"`, `"pdf"`, `"eml"`, `"msg"`, `"mbox"`, `"zip"`, `"tar"`, `"7z"`).
    pub format: String,
    /// Finding kinds supported and checked by this format's provider.
    pub supported_kinds: Vec<FindingKind>,
    /// Optional notes on format-specific coverage details.
    pub notes: Option<String>,
}

impl FormatCoverage {
    /// Return the [`FormatCoverage`] declaration for `format_id`.
    #[must_use]
    pub fn for_format(format_id: &str) -> Self {
        let lower = format_id.to_ascii_lowercase();
        for entry in Self::published_coverage_table() {
            if entry.format == lower {
                return entry;
            }
        }
        Self {
            format: lower,
            supported_kinds: vec![
                FindingKind::ZeroWidthCharacters,
                FindingKind::AuthorAndModificationHistory,
                FindingKind::CustomPropertiesOrVariables,
            ],
            notes: Some("Generic metadata and text inspection".to_string()),
        }
    }

    /// Return the complete published coverage table across all major Prism formats.
    #[must_use]
    pub fn published_coverage_table() -> Vec<Self> {
        vec![
            Self {
                format: "docx".to_string(),
                supported_kinds: vec![
                    FindingKind::TrackedInsertion,
                    FindingKind::TrackedDeletion,
                    FindingKind::TrackedMove,
                    FindingKind::TrackedFormatChange,
                    FindingKind::Comment,
                    FindingKind::HiddenText,
                    FindingKind::WhiteOrLowContrastText,
                    FindingKind::TinyText,
                    FindingKind::OffPageOrOffSlideText,
                    FindingKind::CoveredText,
                    FindingKind::ZeroWidthCharacters,
                    FindingKind::MacroCode,
                    FindingKind::ActiveXControl,
                    FindingKind::EmbeddedOleObject,
                    FindingKind::ExternalLinkOrConnection,
                    FindingKind::DynamicField,
                    FindingKind::EmbeddedFile,
                    FindingKind::AuthorAndModificationHistory,
                    FindingKind::CustomPropertiesOrVariables,
                    FindingKind::RsidDiversity,
                    FindingKind::XmpHistory,
                ],
                notes: Some("Full OOXML Word model + positioned layout inspection".to_string()),
            },
            Self {
                format: "doc".to_string(),
                supported_kinds: vec![
                    FindingKind::TrackedInsertion,
                    FindingKind::TrackedDeletion,
                    FindingKind::Comment,
                    FindingKind::HiddenText,
                    FindingKind::WhiteOrLowContrastText,
                    FindingKind::TinyText,
                    FindingKind::ZeroWidthCharacters,
                    FindingKind::MacroCode,
                    FindingKind::EmbeddedOleObject,
                    FindingKind::ExternalLinkOrConnection,
                    FindingKind::DynamicField,
                    FindingKind::AuthorAndModificationHistory,
                    FindingKind::CustomPropertiesOrVariables,
                ],
                notes: Some("MS-DOC binary FIB/piece-table/CFB stream inspection".to_string()),
            },
            Self {
                format: "pptx".to_string(),
                supported_kinds: vec![
                    FindingKind::Comment,
                    FindingKind::WhiteOrLowContrastText,
                    FindingKind::TinyText,
                    FindingKind::OffPageOrOffSlideText,
                    FindingKind::CoveredText,
                    FindingKind::ZeroWidthCharacters,
                    FindingKind::HiddenSlide,
                    FindingKind::SpeakerNotes,
                    FindingKind::MacroCode,
                    FindingKind::ActiveXControl,
                    FindingKind::EmbeddedOleObject,
                    FindingKind::ExternalLinkOrConnection,
                    FindingKind::EmbeddedFile,
                    FindingKind::AuthorAndModificationHistory,
                    FindingKind::CustomPropertiesOrVariables,
                ],
                notes: Some("Full PPTX presentation + slide layout inspection".to_string()),
            },
            Self {
                format: "ppt".to_string(),
                supported_kinds: vec![
                    FindingKind::Comment,
                    FindingKind::HiddenSlide,
                    FindingKind::SpeakerNotes,
                    FindingKind::ZeroWidthCharacters,
                    FindingKind::MacroCode,
                    FindingKind::EmbeddedOleObject,
                    FindingKind::AuthorAndModificationHistory,
                ],
                notes: Some("Legacy MS-PPT binary record + CFB stream inspection".to_string()),
            },
            Self {
                format: "xlsx".to_string(),
                supported_kinds: vec![
                    FindingKind::Comment,
                    FindingKind::WhiteOrLowContrastText,
                    FindingKind::TinyText,
                    FindingKind::CustomSuppressedCell,
                    FindingKind::ZeroWidthCharacters,
                    FindingKind::HiddenSheet,
                    FindingKind::HiddenRowsOrColumns,
                    FindingKind::CollapsedOutlineGroup,
                    FindingKind::HiddenDefinedName,
                    FindingKind::MacroCode,
                    FindingKind::ActiveXControl,
                    FindingKind::EmbeddedOleObject,
                    FindingKind::ExternalLinkOrConnection,
                    FindingKind::DynamicField,
                    FindingKind::EmbeddedFile,
                    FindingKind::AuthorAndModificationHistory,
                    FindingKind::CustomPropertiesOrVariables,
                ],
                notes: Some(
                    "Full XLSX workbook + cell/style/sheet forensic inspection".to_string(),
                ),
            },
            Self {
                format: "xls".to_string(),
                supported_kinds: vec![
                    FindingKind::Comment,
                    FindingKind::WhiteOrLowContrastText,
                    FindingKind::TinyText,
                    FindingKind::CustomSuppressedCell,
                    FindingKind::ZeroWidthCharacters,
                    FindingKind::HiddenSheet,
                    FindingKind::HiddenRowsOrColumns,
                    FindingKind::CollapsedOutlineGroup,
                    FindingKind::HiddenDefinedName,
                    FindingKind::MacroCode,
                    FindingKind::EmbeddedOleObject,
                    FindingKind::ExternalLinkOrConnection,
                    FindingKind::DynamicField,
                    FindingKind::AuthorAndModificationHistory,
                ],
                notes: Some("BIFF8 XLS workbook + CFB stream inspection".to_string()),
            },
            Self {
                format: "pdf".to_string(),
                supported_kinds: vec![
                    FindingKind::PdfAnnotation,
                    FindingKind::WhiteOrLowContrastText,
                    FindingKind::TinyText,
                    FindingKind::OffPageOrOffSlideText,
                    FindingKind::CoveredText,
                    FindingKind::ZeroWidthCharacters,
                    FindingKind::HiddenPdfLayer,
                    FindingKind::PdfJavaScript,
                    FindingKind::PdfLaunchAction,
                    FindingKind::EmbeddedFile,
                    FindingKind::ExternalLinkOrConnection,
                    FindingKind::AuthorAndModificationHistory,
                    FindingKind::XmpHistory,
                    FindingKind::IncrementalPdfUpdates,
                ],
                notes: Some(
                    "PDF object tree (lopdf) + positioned page model (hayro) inspection"
                        .to_string(),
                ),
            },
            Self {
                format: "eml".to_string(),
                supported_kinds: vec![
                    FindingKind::WhiteOrLowContrastText,
                    FindingKind::ZeroWidthCharacters,
                    FindingKind::ExternalLinkOrConnection,
                    FindingKind::EmbeddedFile,
                    FindingKind::RemoteEmailImage,
                    FindingKind::AuthorAndModificationHistory,
                    FindingKind::InconsistentEmailHeaders,
                ],
                notes: Some(
                    "RFC 5322 / MIME header, HTML body, and attachment inspection".to_string(),
                ),
            },
            Self {
                format: "msg".to_string(),
                supported_kinds: vec![
                    FindingKind::WhiteOrLowContrastText,
                    FindingKind::ZeroWidthCharacters,
                    FindingKind::ExternalLinkOrConnection,
                    FindingKind::EmbeddedFile,
                    FindingKind::RemoteEmailImage,
                    FindingKind::AuthorAndModificationHistory,
                    FindingKind::InconsistentEmailHeaders,
                ],
                notes: Some(
                    "MAPI MSG property, transport header, HTML, and attachment inspection"
                        .to_string(),
                ),
            },
            Self {
                format: "mbox".to_string(),
                supported_kinds: vec![
                    FindingKind::WhiteOrLowContrastText,
                    FindingKind::ZeroWidthCharacters,
                    FindingKind::ExternalLinkOrConnection,
                    FindingKind::EmbeddedFile,
                    FindingKind::RemoteEmailImage,
                    FindingKind::AuthorAndModificationHistory,
                    FindingKind::InconsistentEmailHeaders,
                ],
                notes: Some("MBOX mailbox message and attachment inspection".to_string()),
            },
            Self {
                format: "zip".to_string(),
                supported_kinds: vec![
                    FindingKind::EmbeddedFile,
                    FindingKind::MacroCode,
                    FindingKind::AuthorAndModificationHistory,
                ],
                notes: Some("ZIP archive member and hidden/dot-file inspection".to_string()),
            },
            Self {
                format: "tar".to_string(),
                supported_kinds: vec![
                    FindingKind::EmbeddedFile,
                    FindingKind::AuthorAndModificationHistory,
                ],
                notes: Some("TAR archive member and hidden/dot-file inspection".to_string()),
            },
            Self {
                format: "7z".to_string(),
                supported_kinds: vec![
                    FindingKind::EmbeddedFile,
                    FindingKind::AuthorAndModificationHistory,
                ],
                notes: Some("7z archive member and hidden/dot-file inspection".to_string()),
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_table_covers_all_core_formats() {
        let table = FormatCoverage::published_coverage_table();
        for fmt in [
            "docx", "doc", "pptx", "ppt", "xlsx", "xls", "pdf", "eml", "msg", "mbox", "zip", "tar",
            "7z",
        ] {
            assert!(
                table
                    .iter()
                    .any(|c| c.format == fmt && !c.supported_kinds.is_empty()),
                "missing coverage entry for {fmt}"
            );
        }
    }

    #[test]
    fn bounded_excerpt_truncates_long_strings() {
        let long = "a".repeat(300);
        let exc = bounded_excerpt(&long);
        assert_eq!(exc.chars().count(), MAX_EXCERPT_CHARS + 1);
        assert!(exc.ends_with('…'));
    }
}

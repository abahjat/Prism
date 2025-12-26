// SPDX-License-Identifier: AGPL-3.0-only
//! # Unified Document Model (UDM)
//!
//! The UDM is the core intermediate representation that all document formats
//! are parsed into. This enables format-agnostic processing, rendering, and
//! manipulation.
//!
//! ## Design Principles
//!
//! 1. **Format Agnostic**: Any document format can be represented
//! 2. **Lossless**: Preserve as much information as possible from source
//! 3. **Efficient**: Minimize memory usage, support streaming
//! 4. **Extensible**: Easy to add new content types
//!
//! ## Structure
//!
//! ```text
//! Document
//! ├── Metadata (title, author, dates, custom properties)
//! ├── Pages[]
//! │   ├── Dimensions
//! │   ├── Content Blocks[]
//! │   │   ├── Text (runs, styles, positions)
//! │   │   ├── Images (embedded, linked)
//! │   │   ├── Tables (rows, cols, cells)
//! │   │   └── Vectors (paths, shapes)
//! │   └── Annotations
//! ├── Styles (fonts, colors, paragraph styles)
//! ├── Resources (fonts, images, embeddings)
//! └── Structure (headings, TOC, bookmarks)
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::format::Format;
use crate::metadata::Metadata;

/// A parsed document in the Unified Document Model format.
///
/// This is the central data structure that all format parsers produce.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    /// Unique identifier for this document instance
    pub id: Uuid,

    /// Information about the source file
    pub source: SourceInfo,

    /// Document metadata (title, author, dates, etc.)
    pub metadata: Metadata,

    /// Document pages (or equivalent for non-paged formats)
    pub pages: Vec<Page>,

    /// Style definitions used throughout the document
    pub styles: StyleSheet,

    /// Resources referenced by the document (fonts, images, etc.)
    pub resources: ResourceStore,

    /// Document structure (headings, bookmarks, TOC)
    pub structure: DocumentStructure,

    /// Embedded files/attachments
    pub attachments: Vec<Attachment>,
}

impl Document {
    /// Create a new empty document
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            source: SourceInfo::default(),
            metadata: Metadata::default(),
            pages: Vec::new(),
            styles: StyleSheet::default(),
            resources: ResourceStore::default(),
            structure: DocumentStructure::default(),
            attachments: Vec::new(),
        }
    }

    /// Create a document builder for fluent construction
    #[must_use]
    pub fn builder() -> DocumentBuilder {
        DocumentBuilder::new()
    }

    /// Get the total number of pages
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Get a specific page by number (1-indexed)
    #[must_use]
    pub fn page(&self, number: usize) -> Option<&Page> {
        if number == 0 {
            return None;
        }
        self.pages.get(number - 1)
    }

    /// Extract all text content from the document
    #[must_use]
    pub fn extract_text(&self) -> String {
        self.pages
            .iter()
            .map(|page| page.extract_text())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Get the total word count
    #[must_use]
    pub fn word_count(&self) -> usize {
        self.extract_text().split_whitespace().count()
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for constructing documents fluently
#[derive(Debug, Default)]
pub struct DocumentBuilder {
    document: Document,
}

impl DocumentBuilder {
    /// Create a new document builder
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the document metadata
    #[must_use]
    pub fn metadata(mut self, metadata: Metadata) -> Self {
        self.document.metadata = metadata;
        self
    }

    /// Add a page to the document
    #[must_use]
    pub fn page(mut self, page: Page) -> Self {
        self.document.pages.push(page);
        self
    }

    /// Set the source information
    #[must_use]
    pub fn source(mut self, source: SourceInfo) -> Self {
        self.document.source = source;
        self
    }

    /// Build the final document
    #[must_use]
    pub fn build(self) -> Document {
        self.document
    }
}

/// Information about the source file
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceInfo {
    /// Original filename (if known)
    pub filename: Option<String>,

    /// Detected format
    pub format: Option<Format>,

    /// File size in bytes
    pub size: Option<u64>,

    /// Hash of the original content (for verification)
    pub hash: Option<String>,

    /// When the document was parsed
    pub parsed_at: Option<DateTime<Utc>>,
}

/// A single page in the document
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    /// Page number (1-indexed)
    pub number: u32,

    /// Page dimensions
    pub dimensions: Dimensions,

    /// Content blocks on this page
    pub content: Vec<ContentBlock>,

    /// Annotations on this page
    pub annotations: Vec<Annotation>,

    /// Page-specific metadata
    pub metadata: PageMetadata,
}

impl Page {
    /// Create a new page with the given number and dimensions
    #[must_use]
    pub fn new(number: u32, dimensions: Dimensions) -> Self {
        Self {
            number,
            dimensions,
            content: Vec::new(),
            annotations: Vec::new(),
            metadata: PageMetadata::default(),
        }
    }

    /// Extract all text from this page
    #[must_use]
    pub fn extract_text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.extract_text()),
                ContentBlock::Table(table) => Some(table.extract_text()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Add a content block to this page
    pub fn add_content(&mut self, block: ContentBlock) {
        self.content.push(block);
    }
}

/// Page dimensions
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Dimensions {
    /// Width in points (1 point = 1/72 inch)
    pub width: f64,

    /// Height in points
    pub height: f64,
}

impl Dimensions {
    /// Standard US Letter size (8.5" x 11")
    pub const LETTER: Self = Self {
        width: 612.0,
        height: 792.0,
    };

    /// Standard A4 size (210mm x 297mm)
    pub const A4: Self = Self {
        width: 595.28,
        height: 841.89,
    };

    /// Create custom dimensions
    #[must_use]
    pub fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }

    /// Create dimensions from inches
    #[must_use]
    pub fn from_inches(width: f64, height: f64) -> Self {
        Self {
            width: width * 72.0,
            height: height * 72.0,
        }
    }

    /// Create dimensions from millimeters
    #[must_use]
    pub fn from_mm(width: f64, height: f64) -> Self {
        Self {
            width: width * 2.834645669,
            height: height * 2.834645669,
        }
    }
}

impl Default for Dimensions {
    fn default() -> Self {
        Self::LETTER
    }
}

/// A content block within a page
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    /// Text content
    Text(TextBlock),

    /// Image content
    Image(ImageBlock),

    /// Table content
    Table(TableBlock),

    /// Vector graphics
    Vector(VectorBlock),

    /// Container for nested content
    Container(ContainerBlock),
}

/// Visual style for a shape or block
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShapeStyle {
    /// Fill color (hex or named)
    pub fill_color: Option<String>,
    /// Stroke/Border color
    pub stroke_color: Option<String>,
    /// Stroke width in points
    pub stroke_width: Option<f64>,
}

/// A block of text content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextBlock {
    /// Bounding box on the page
    pub bounds: Rect,

    /// Text runs within this block
    pub runs: Vec<TextRun>,

    /// Paragraph style reference
    pub paragraph_style: Option<String>,

    /// Visual style of the text box container
    #[serde(default)]
    pub style: ShapeStyle,

    /// Rotation in degrees
    #[serde(default)]
    pub rotation: f64,
}

impl TextBlock {
    /// Create a new text block
    #[must_use]
    pub fn new(bounds: Rect) -> Self {
        Self {
            bounds,
            runs: Vec::new(),
            paragraph_style: None,
            style: ShapeStyle::default(),
            rotation: 0.0,
        }
    }

    /// Add a text run
    pub fn add_run(&mut self, run: TextRun) {
        self.runs.push(run);
    }

    /// Extract plain text from this block
    #[must_use]
    pub fn extract_text(&self) -> String {
        self.runs
            .iter()
            .map(|run| run.text.as_str())
            .collect::<Vec<_>>()
            .join("")
    }
}

/// A run of text with consistent styling
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextRun {
    /// The text content
    pub text: String,

    /// Style for this run
    pub style: TextStyle,

    /// Bounding box (if available)
    pub bounds: Option<Rect>,

    /// Individual character positions (for precise selection/highlighting)
    pub char_positions: Option<Vec<Point>>,
}

impl TextRun {
    /// Create a new text run with default styling
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: TextStyle::default(),
            bounds: None,
            char_positions: None,
        }
    }

    /// Create a text run with specific style
    #[must_use]
    pub fn with_style(text: impl Into<String>, style: TextStyle) -> Self {
        Self {
            text: text.into(),
            style,
            bounds: None,
            char_positions: None,
        }
    }
}

/// Text styling properties
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TextStyle {
    /// Font family name
    pub font_family: Option<String>,

    /// Font size in points
    pub font_size: Option<f64>,

    /// Bold weight
    pub bold: bool,

    /// Italic style
    pub italic: bool,

    /// Underline
    pub underline: bool,

    /// Strikethrough
    pub strikethrough: bool,

    /// Text color (hex or named)
    pub color: Option<String>,

    /// Background/highlight color
    pub background_color: Option<String>,
}

/// An image block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageBlock {
    /// Bounding box on the page
    pub bounds: Rect,

    /// Reference to image resource
    pub resource_id: String,

    /// Alt text for accessibility
    pub alt_text: Option<String>,

    /// Image format (JPEG, PNG, etc.)
    pub format: Option<String>,

    /// Original dimensions (before scaling)
    pub original_size: Option<Dimensions>,

    /// Visual style of the image container
    #[serde(default)]
    pub style: ShapeStyle,

    /// Rotation in degrees
    #[serde(default)]
    pub rotation: f64,
}

/// A table block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableBlock {
    /// Bounding box on the page
    pub bounds: Rect,

    /// Table rows
    pub rows: Vec<TableRow>,

    /// Number of columns
    pub column_count: usize,

    /// Visual style of the table container
    #[serde(default)]
    pub style: ShapeStyle,

    /// Rotation in degrees
    #[serde(default)]
    pub rotation: f64,
}

impl TableBlock {
    /// Create a new empty table
    #[must_use]
    pub fn new(bounds: Rect, column_count: usize) -> Self {
        Self {
            bounds,
            rows: Vec::new(),
            column_count,
            style: ShapeStyle::default(),
            rotation: 0.0,
        }
    }

    /// Add a row to the table
    pub fn add_row(&mut self, row: TableRow) {
        self.rows.push(row);
    }

    /// Extract text from the table
    #[must_use]
    pub fn extract_text(&self) -> String {
        self.rows
            .iter()
            .map(|row| {
                row.cells
                    .iter()
                    .map(|cell| cell.extract_text())
                    .collect::<Vec<_>>()
                    .join("\t")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A table row
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableRow {
    /// Cells in this row
    pub cells: Vec<TableCell>,

    /// Row height (if specified)
    pub height: Option<f64>,
}

/// A table cell
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableCell {
    /// Content blocks within the cell
    pub content: Vec<ContentBlock>,

    /// Number of columns this cell spans
    pub col_span: usize,

    /// Number of rows this cell spans
    pub row_span: usize,

    /// Background color (hex or named)
    pub background_color: Option<String>,
}

impl TableCell {
    /// Extract text from the cell
    #[must_use]
    pub fn extract_text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.extract_text()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Vector graphics block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorBlock {
    /// Bounding box
    pub bounds: Rect,

    /// Vector paths
    pub paths: Vec<VectorPath>,
}

/// A vector path
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorPath {
    /// Path commands (MoveTo, LineTo, CurveTo, etc.)
    pub commands: Vec<PathCommand>,

    /// Fill color
    pub fill: Option<String>,

    /// Stroke color
    pub stroke: Option<String>,

    /// Stroke width
    pub stroke_width: Option<f64>,
}

/// Path drawing commands
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PathCommand {
    /// Move to point
    MoveTo(Point),
    /// Line to point
    LineTo(Point),
    /// Cubic bezier curve
    CurveTo { cp1: Point, cp2: Point, end: Point },
    /// Quadratic bezier curve
    QuadTo { cp: Point, end: Point },
    /// Close the path
    Close,
}

/// Container for nested content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerBlock {
    /// Bounding box
    pub bounds: Rect,

    /// Nested content blocks
    pub children: Vec<ContentBlock>,

    /// Container type (e.g., "group", "frame", "text-box")
    pub container_type: Option<String>,
}

/// A rectangle (bounding box)
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Rect {
    /// X coordinate of top-left corner
    pub x: f64,
    /// Y coordinate of top-left corner
    pub y: f64,
    /// Width
    pub width: f64,
    /// Height
    pub height: f64,
}

impl Rect {
    /// Create a new rectangle
    #[must_use]
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Check if this rect contains a point
    #[must_use]
    pub fn contains(&self, point: Point) -> bool {
        point.x >= self.x
            && point.x <= self.x + self.width
            && point.y >= self.y
            && point.y <= self.y + self.height
    }
}

/// A 2D point
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Point {
    /// X coordinate
    pub x: f64,
    /// Y coordinate
    pub y: f64,
}

impl Point {
    /// Create a new point
    #[must_use]
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// An annotation on a page
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Annotation {
    /// Unique identifier
    pub id: Uuid,

    /// Annotation type
    pub annotation_type: AnnotationType,

    /// Position on page
    pub bounds: Rect,

    /// Content (for comments, etc.)
    pub content: Option<String>,

    /// Author
    pub author: Option<String>,

    /// Creation date
    pub created: Option<DateTime<Utc>>,

    /// Color
    pub color: Option<String>,
}

/// Types of annotations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AnnotationType {
    /// Text highlight
    Highlight,
    /// Underline
    Underline,
    /// Strikeout
    Strikeout,
    /// Comment/note
    Comment,
    /// Redaction
    Redaction,
    /// Stamp
    Stamp,
    /// Freehand drawing
    Ink,
    /// Link
    Link { url: String },
}

/// Page-specific metadata
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PageMetadata {
    /// Page label (e.g., "i", "ii", "1", "2")
    pub label: Option<String>,

    /// Rotation in degrees (0, 90, 180, 270)
    pub rotation: i32,
}

/// Document stylesheet containing style definitions
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StyleSheet {
    /// Named text styles
    pub text_styles: Vec<NamedStyle<TextStyle>>,

    /// Named paragraph styles
    pub paragraph_styles: Vec<NamedStyle<ParagraphStyle>>,
}

/// A named style
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedStyle<T> {
    /// Style name/identifier
    pub name: String,

    /// Style definition
    pub style: T,
}

/// Paragraph styling properties
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParagraphStyle {
    /// Text alignment
    pub alignment: TextAlignment,

    /// Line height multiplier
    pub line_height: Option<f64>,

    /// Space before paragraph (points)
    pub space_before: Option<f64>,

    /// Space after paragraph (points)
    pub space_after: Option<f64>,

    /// First line indent (points)
    pub first_line_indent: Option<f64>,

    /// Left indent (points)
    pub left_indent: Option<f64>,

    /// Right indent (points)
    pub right_indent: Option<f64>,
}

/// Text alignment options
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub enum TextAlignment {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

/// Store for document resources (fonts, images, etc.)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceStore {
    /// Embedded images
    pub images: Vec<ImageResource>,

    /// Font information
    pub fonts: Vec<FontResource>,
}

/// An embedded image resource
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageResource {
    /// Resource identifier (referenced by ImageBlock)
    pub id: String,

    /// MIME type
    pub mime_type: String,

    /// Image data (base64 encoded for serialization)
    pub data: Option<Vec<u8>>,

    /// External URL (if not embedded)
    pub url: Option<String>,

    /// Width in pixels
    pub width: u32,

    /// Height in pixels
    pub height: u32,
}

/// Font resource information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FontResource {
    /// Font family name
    pub family: String,

    /// Font style (Regular, Bold, Italic, etc.)
    pub style: String,

    /// Whether font is embedded
    pub embedded: bool,

    /// Font data (if embedded)
    pub data: Option<Vec<u8>>,
}

/// Document structure (headings, bookmarks, TOC)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocumentStructure {
    /// Document outline/bookmarks
    pub outline: Vec<OutlineItem>,

    /// Table of contents entries
    pub toc: Vec<TocEntry>,

    /// Heading structure
    pub headings: Vec<Heading>,
}

/// An outline/bookmark item
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutlineItem {
    /// Title
    pub title: String,

    /// Target page number
    pub page: u32,

    /// Y position on page
    pub y_position: Option<f64>,

    /// Child items
    pub children: Vec<OutlineItem>,
}

/// Table of contents entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TocEntry {
    /// Entry title
    pub title: String,

    /// Page number
    pub page: u32,

    /// Heading level (1-6)
    pub level: u8,
}

/// A heading in the document
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heading {
    /// Heading text
    pub text: String,

    /// Heading level (1-6)
    pub level: u8,

    /// Page number
    pub page: u32,

    /// Position on page
    pub bounds: Option<Rect>,
}

/// An embedded file/attachment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    /// Filename
    pub filename: String,

    /// MIME type
    pub mime_type: Option<String>,

    /// Description
    pub description: Option<String>,

    /// File data
    pub data: Vec<u8>,

    /// Creation date
    pub created: Option<DateTime<Utc>>,

    /// Modification date
    pub modified: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_document_creation() {
        let doc = Document::new();
        assert_eq!(doc.page_count(), 0);
    }

    #[test]
    fn test_document_builder() {
        let doc = Document::builder()
            .metadata(Metadata {
                title: Some("Test Document".to_string()),
                ..Default::default()
            })
            .page(Page::new(1, Dimensions::LETTER))
            .page(Page::new(2, Dimensions::LETTER))
            .build();

        assert_eq!(doc.page_count(), 2);
        assert_eq!(doc.metadata.title, Some("Test Document".to_string()));
    }

    #[test]
    fn test_text_extraction() {
        let mut page = Page::new(1, Dimensions::LETTER);

        let mut text_block = TextBlock::new(Rect::default());
        text_block.add_run(TextRun::new("Hello, "));
        text_block.add_run(TextRun::new("World!"));

        page.add_content(ContentBlock::Text(text_block));

        assert_eq!(page.extract_text(), "Hello, World!");
    }

    #[test]
    fn test_dimensions() {
        let letter = Dimensions::LETTER;
        assert!((letter.width - 612.0).abs() < 0.01);
        assert!((letter.height - 792.0).abs() < 0.01);

        let from_inches = Dimensions::from_inches(8.5, 11.0);
        assert!((from_inches.width - 612.0).abs() < 0.01);
        assert!((from_inches.height - 792.0).abs() < 0.01);
    }

    #[test]
    fn test_rect_contains() {
        let rect = Rect::new(10.0, 10.0, 100.0, 50.0);

        assert!(rect.contains(Point::new(50.0, 30.0)));
        assert!(rect.contains(Point::new(10.0, 10.0)));
        assert!(!rect.contains(Point::new(5.0, 30.0)));
        assert!(!rect.contains(Point::new(50.0, 100.0)));
    }
}

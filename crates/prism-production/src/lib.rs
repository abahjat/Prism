// SPDX-License-Identifier: AGPL-3.0-only
//! # Prism Production (`prism-production`)
//!
//! Deterministic, resumable eDiscovery image and PDF productions with continuous Bates numbering,
//! non-overlapping 6-position endorsements, configurable slip sheets, per-document extracted text,
//! Concordance `.dat` / Opticon `.opt` / CSV load files, reference load-file readers, and
//! content-addressed JSON manifests.

#![warn(missing_docs)]
#![warn(clippy::all)]
#![warn(clippy::pedantic)]
#![allow(
    clippy::chunks_exact_to_as_chunks,
    clippy::module_name_repetitions,
    clippy::too_many_arguments,
    clippy::too_many_lines
)]

use prism_core::ENGINE_VERSION;
use prism_core::error::{Error, Result};
use prism_core::hash::{md5_hex, sha1_hex, sha256_hex};
use prism_core::metadata::Metadata;
use prism_core::review::SourceRef;
use prism_layout::{
    Affine, ClipPath, Cluster, Decorations, FillRule, FontId, FontSourceKind, FontStore, GlyphRun,
    GroupItem, Item, LayoutDocument, LayoutOptions, LayoutPage, LineCap, LineJoin, PathCommand,
    PathItem, Point, PositionedGlyph, Rgba, Size, StrokeStyle,
};
use prism_parsers::pipeline::{open_and_layout, open_and_layout_page_range};
use prism_render::pdf::{PdfExportOptions, export_document_pdf};
use prism_render::raster::{RasterImage, RasterOptions, render_page_png, render_page_rgba};
use prism_review::{
    AnnotationKind, AnnotationSidecar, apply_redactions, apply_workbook_redactions,
    has_approved_cell_redactions, scrub_metadata, verify_redacted_output,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Cursor;
use std::path::Path;
use tiff::encoder::{Compression as TiffCompression, DeflateLevel, TiffEncoder, colortype};

/// Current schema version for [`ProductionManifest`].
pub const PRODUCTION_MANIFEST_SCHEMA_VERSION: u32 = 1;

const BUNDLED_SANS_REGULAR: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationSans-Regular.ttf");
const BUNDLED_SANS_BOLD: &[u8] = include_bytes!("../../../assets/fonts/LiberationSans-Bold.ttf");

/// Bates numbering configuration (`{prefix}{padded_number}{suffix}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatesConfig {
    /// Leading prefix (e.g. `"PRISM"`, `"ABC_"`).
    pub prefix: String,
    /// Starting 1-based Bates integer (e.g. `1`).
    pub start_number: u64,
    /// Zero-padded digit width (e.g. `7` -> `"0000001"`).
    pub padding: usize,
    /// Optional trailing suffix (e.g. `""` or `"_CONF"`).
    pub suffix: String,
}

impl Default for BatesConfig {
    fn default() -> Self {
        Self {
            prefix: "PRISM".to_string(),
            start_number: 1,
            padding: 7,
            suffix: String::new(),
        }
    }
}

impl BatesConfig {
    /// Format a numeric Bates index into its canonical string (e.g. `"PRISM0000001"`).
    #[must_use]
    pub fn format_bates(&self, number: u64) -> String {
        format!(
            "{}{:0width$}{}",
            self.prefix,
            number,
            self.suffix,
            width = self.padding
        )
    }

    /// Parse a formatted Bates string back into its numeric value if it matches `prefix` and `suffix`.
    #[must_use]
    pub fn parse_bates_number(&self, formatted: &str) -> Option<u64> {
        let mid = formatted
            .strip_prefix(&self.prefix)?
            .strip_suffix(&self.suffix)?;
        mid.parse::<u64>().ok()
    }
}

/// Six-position page endorsement templates (`header_left/center/right`, `footer_left/center/right`).
///
/// Supported tokens: `{bates}`, `{doc_id}`, `{page}`, `{page_count}`, `{confidentiality}`, `{date}`.
/// When any header or footer endorsement is non-empty, page content is scaled into a safe inner
/// margin so endorsements **never overlap** document content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EndorsementConfig {
    /// Top-left endorsement template.
    pub header_left: Option<String>,
    /// Top-center endorsement template.
    pub header_center: Option<String>,
    /// Top-right endorsement template.
    pub header_right: Option<String>,
    /// Bottom-left endorsement template (commonly `Some("{confidentiality}".to_string())`).
    pub footer_left: Option<String>,
    /// Bottom-center endorsement template.
    pub footer_center: Option<String>,
    /// Bottom-right endorsement template (commonly `Some("{bates}".to_string())`).
    pub footer_right: Option<String>,
    /// Font size in points for endorsement text (default `8.5`).
    pub font_size_pt: f32,
}

impl Default for EndorsementConfig {
    fn default() -> Self {
        Self {
            header_left: None,
            header_center: None,
            header_right: None,
            footer_left: Some("{confidentiality}".to_string()),
            footer_center: None,
            footer_right: Some("{bates}".to_string()),
            font_size_pt: 8.5,
        }
    }
}

impl EndorsementConfig {
    /// Whether at least one header position has a non-empty template.
    #[must_use]
    pub fn has_header(&self) -> bool {
        [&self.header_left, &self.header_center, &self.header_right]
            .into_iter()
            .flatten()
            .any(|s| !s.trim().is_empty())
    }

    /// Whether at least one footer position has a non-empty template.
    #[must_use]
    pub fn has_footer(&self) -> bool {
        [&self.footer_left, &self.footer_center, &self.footer_right]
            .into_iter()
            .flatten()
            .any(|s| !s.trim().is_empty())
    }
}

/// Values substituted into endorsement tokens on a single page.
#[derive(Debug, Clone)]
pub struct EndorsementTokens<'a> {
    /// Page Bates number (`{bates}`).
    pub bates: &'a str,
    /// Document control ID (`{doc_id}`).
    pub doc_id: &'a str,
    /// 1-based page number within the document (`{page}`).
    pub page: usize,
    /// Total pages in the document (`{page_count}`).
    pub page_count: usize,
    /// Confidentiality designation (`{confidentiality}`).
    pub confidentiality: &'a str,
    /// Production date string from options (`{date}`).
    pub date: &'a str,
}

impl EndorsementTokens<'_> {
    /// Expand `{bates}`, `{doc_id}`, `{page}`, `{page_count}`, `{confidentiality}`, and `{date}` in `template`.
    #[must_use]
    pub fn expand(&self, template: &str) -> String {
        template
            .replace("{bates}", self.bates)
            .replace("{doc_id}", self.doc_id)
            .replace("{page}", &self.page.to_string())
            .replace("{page_count}", &self.page_count.to_string())
            .replace("{confidentiality}", self.confidentiality)
            .replace("{date}", self.date)
            .trim()
            .to_string()
    }
}

/// Reason category for a single-page placeholder slip sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason_type", rename_all = "snake_case")]
pub enum SlipSheetKind {
    /// Document produced natively only (`NativeOnly`).
    NativeOnly,
    /// Document withheld for legal privilege (`Privileged`).
    Privileged,
    /// Document withheld for other responsiveness/privacy grounds (`Withheld`).
    Withheld,
    /// Document failed to parse or render (`Failed`).
    Failed {
        /// Structured error summary.
        error: String,
    },
}

/// Configuration for a single-page placeholder slip sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlipSheetSpec {
    /// Slip sheet reason category.
    pub kind: SlipSheetKind,
    /// Banner heading (e.g. `"DOCUMENT PRODUCED IN NATIVE FORMAT"` or `"WITHHELD FOR PRIVILEGE"`).
    pub title: String,
    /// Additional custom lines displayed on the slip sheet.
    pub custom_lines: Vec<String>,
}

impl SlipSheetSpec {
    /// Create a standard `NativeOnly` slip sheet specification.
    #[must_use]
    pub fn native_only() -> Self {
        Self {
            kind: SlipSheetKind::NativeOnly,
            title: "DOCUMENT PRODUCED IN NATIVE FORMAT".to_string(),
            custom_lines: Vec::new(),
        }
    }

    /// Create a standard `Privileged` slip sheet specification.
    #[must_use]
    pub fn privileged(reason: impl Into<String>) -> Self {
        Self {
            kind: SlipSheetKind::Privileged,
            title: "DOCUMENT WITHHELD — PRIVILEGED".to_string(),
            custom_lines: vec![reason.into()],
        }
    }
}

/// Image output mode for a production.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductionImageFormat {
    /// Do not emit raster image files.
    None,
    /// One single-page TIFF per page (`IMAGES/{bates}.tif`), using grayscale/bitonal encoding for monochrome pages and RGBA Deflate for color pages.
    SinglePageTiff,
    /// One multi-page TIFF per document (`IMAGES/{begin_bates}.tif`).
    MultiPageTiff,
    /// One single-page PNG per page (`IMAGES/{bates}.png`).
    SinglePagePng,
}

/// Searchable PDF output mode for a production.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ProductionPdfMode {
    /// Do not emit PDF files.
    None,
    /// Emit one searchable PDF per document (`IMAGES/{begin_bates}.pdf`).
    PerDocument {
        /// Enable PDF/A-2b archival conformance settings.
        pdf_a2b: bool,
    },
    /// Emit both per-document PDFs and a combined volume PDF (`IMAGES/{volume_name}.pdf`).
    CombinedVolume {
        /// Enable PDF/A-2b archival conformance settings.
        pdf_a2b: bool,
    },
}

/// How Excel spreadsheets are produced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpreadsheetProductionMode {
    /// Render spreadsheets using their Phase 3 paginated print layout.
    #[default]
    PrintLayout,
    /// Produce spreadsheets as native files accompanied by a 1-page `NativeOnly` slip sheet.
    NativeWithSlipSheet,
}

/// Load-file generation options (`Concordance .dat`, `Opticon .opt`, `CSV .csv`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadFileConfig {
    /// Emit a Concordance `.dat` file (`LOADFILES/{volume}.dat`).
    pub concordance_dat: bool,
    /// Emit an Opticon `.opt` image cross-reference file (`LOADFILES/{volume}.opt`).
    pub opticon_opt: bool,
    /// Emit a standard RFC 4180 `.csv` metadata load file (`LOADFILES/{volume}.csv`).
    pub csv: bool,
    /// Field delimiter character for Concordance `.dat` (default `'\u{0014}'` DC4).
    pub field_delimiter: char,
    /// Quote/text qualifier character for Concordance `.dat` (default `'\u{00FE}'` thorn `þ`).
    pub quote_delimiter: char,
    /// In-field newline replacement character for Concordance `.dat` (default `'\u{00AE}'` `®`).
    pub newline_replacement: char,
    /// Ordered list of load-file column names.
    pub columns: Vec<String>,
}

impl Default for LoadFileConfig {
    fn default() -> Self {
        Self {
            concordance_dat: true,
            opticon_opt: true,
            csv: true,
            field_delimiter: '\u{0014}',
            quote_delimiter: '\u{00FE}',
            newline_replacement: '\u{00AE}',
            columns: vec![
                "DOCID".to_string(),
                "BEGBATES".to_string(),
                "ENDBATES".to_string(),
                "BEGATTACH".to_string(),
                "ENDATTACH".to_string(),
                "PAGECOUNT".to_string(),
                "CUSTODIAN".to_string(),
                "FILENAME".to_string(),
                "CONFIDENTIALITY".to_string(),
                "MD5".to_string(),
                "SHA1".to_string(),
                "SHA256".to_string(),
                "TITLE".to_string(),
                "AUTHOR".to_string(),
                "DATE".to_string(),
                "TEXTPATH".to_string(),
                "NATIVEPATH".to_string(),
                "REDACTED".to_string(),
            ],
        }
    }
}

/// Complete options governing a production run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProductionOptions {
    /// Volume identifier (e.g. `"VOL001"`).
    pub volume_name: String,
    /// Bates numbering settings.
    pub bates: BatesConfig,
    /// Header/footer endorsement settings.
    pub endorsements: EndorsementConfig,
    /// Raster image output format.
    pub image_format: ProductionImageFormat,
    /// Searchable PDF output mode.
    pub pdf_mode: ProductionPdfMode,
    /// Rasterization DPI for TIFF/PNG outputs (default `300.0`).
    pub dpi: f32,
    /// Emit per-document UTF-8 `.txt` files (`TEXT/{begin_bates}.txt`).
    pub include_text: bool,
    /// Delimit pages in `.txt` files with form-feed (`\x0C`) characters.
    pub page_delimited_text: bool,
    /// Spreadsheet production mode (`PrintLayout` or `NativeWithSlipSheet`).
    pub spreadsheet_mode: SpreadsheetProductionMode,
    /// Load-file configuration.
    pub load_files: LoadFileConfig,
    /// Explicit production date string used in `{date}` tokens and load files (never `now()`).
    pub production_date: String,
    /// Whether to resume from an existing `manifest.json` in `output_dir` if present.
    pub resume: bool,
}

impl Default for ProductionOptions {
    fn default() -> Self {
        Self {
            volume_name: "VOL001".to_string(),
            bates: BatesConfig::default(),
            endorsements: EndorsementConfig::default(),
            image_format: ProductionImageFormat::SinglePageTiff,
            pdf_mode: ProductionPdfMode::PerDocument { pdf_a2b: false },
            dpi: 150.0,
            include_text: true,
            page_delimited_text: true,
            spreadsheet_mode: SpreadsheetProductionMode::PrintLayout,
            load_files: LoadFileConfig::default(),
            production_date: "2026-01-01".to_string(),
            resume: true,
        }
    }
}

impl ProductionOptions {
    /// Compute the deterministic SHA-256 hash of these options.
    #[must_use]
    pub fn options_sha256(&self) -> String {
        let json = serde_json::to_string(self).unwrap_or_default();
        sha256_hex(json.as_bytes())
    }
}

/// A single source document submitted to [`run_production`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductionInput {
    /// Unique document control ID within the production (e.g. `"DOC-0001"`).
    pub doc_id: String,
    /// Original file name (e.g. `"contract.docx"`).
    pub file_name: String,
    /// Raw source document bytes.
    pub bytes: Vec<u8>,
    /// Optional parent document control ID (for email/archive attachment family tracking).
    pub parent_doc_id: Option<String>,
    /// Optional custodian name.
    pub custodian: Option<String>,
    /// Optional confidentiality designation (e.g. `"CONFIDENTIAL"`).
    pub confidentiality: Option<String>,
    /// Optional explicit slip sheet override (for `Privileged`, `Withheld`, or `NativeOnly`).
    pub slip_sheet: Option<SlipSheetSpec>,
    /// Optional annotation/redaction sidecar applied destructively before Bates endorsement.
    pub sidecar: Option<AnnotationSidecar>,
    /// Whether to copy the unmodified native file into `NATIVES/{begin_bates}_{file_name}` (automatically disabled when redactions are applied).
    pub include_native: bool,
    /// Additional host-supplied load-file fields keyed by column name.
    pub extra_fields: BTreeMap<String, String>,
}

impl ProductionInput {
    /// Create a new [`ProductionInput`] with default settings.
    #[must_use]
    pub fn new(doc_id: impl Into<String>, file_name: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            doc_id: doc_id.into(),
            file_name: file_name.into(),
            bytes,
            parent_doc_id: None,
            custodian: None,
            confidentiality: None,
            slip_sheet: None,
            sidecar: None,
            include_native: false,
            extra_fields: BTreeMap::new(),
        }
    }
}

/// Per-document status in a [`ProductionManifest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProducedDocumentStatus {
    /// Rendered and produced normally.
    Completed,
    /// Produced as a placeholder slip sheet (`NativeOnly`, `Privileged`, `Withheld`, or `Failed`).
    SlipSheeted {
        /// Reason category.
        reason: String,
    },
}

/// Record for a single produced document in [`ProductionManifest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProducedDocumentRecord {
    /// Document control ID.
    pub doc_id: String,
    /// Original file name.
    pub file_name: String,
    /// Optional parent document ID.
    pub parent_doc_id: Option<String>,
    /// Starting Bates number (`BEGBATES`).
    pub begin_bates: String,
    /// Ending Bates number (`ENDBATES`).
    pub end_bates: String,
    /// Family attachment range start Bates (`BEGATTACH`).
    pub begin_attach: String,
    /// Family attachment range end Bates (`ENDATTACH`).
    pub end_attach: String,
    /// Number of produced pages.
    pub page_count: usize,
    /// Custodian name.
    pub custodian: String,
    /// Confidentiality string.
    pub confidentiality: String,
    /// Lowercase hex MD5 of source bytes.
    pub md5: String,
    /// Lowercase hex SHA-1 of source bytes.
    pub sha1: String,
    /// Lowercase hex SHA-256 of source bytes.
    pub sha256: String,
    /// Extracted/scrubbed document title.
    pub title: String,
    /// Extracted/scrubbed document author.
    pub author: String,
    /// Relative path to extracted text file (`TEXT/...`), if emitted.
    pub text_path: Option<String>,
    /// Relative path to native file (`NATIVES/...`), if emitted.
    pub native_path: Option<String>,
    /// Per-page relative image paths (`IMAGES/...`).
    pub image_paths: Vec<String>,
    /// Relative path to per-document PDF (`IMAGES/...`), if emitted.
    pub pdf_path: Option<String>,
    /// Whether destructive redactions were applied.
    pub redacted: bool,
    /// Completion status.
    pub status: ProducedDocumentStatus,
    /// Host-supplied extra fields.
    pub extra_fields: BTreeMap<String, String>,
}

/// A single output file recorded in [`ProductionManifest::artifacts`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProducedArtifact {
    /// Relative path from the production root directory (using forward slashes `/`).
    pub relative_path: String,
    /// Lowercase hex SHA-256 of the file bytes.
    pub sha256: String,
    /// Size in bytes.
    pub size_bytes: u64,
}

/// Deterministic JSON manifest describing every produced document and output file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductionManifest {
    /// Schema version ([`PRODUCTION_MANIFEST_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Volume identifier.
    pub volume_name: String,
    /// Prism engine + layout algorithm version.
    pub engine_version: String,
    /// SHA-256 of [`ProductionOptions`].
    pub options_sha256: String,
    /// Production date string from options.
    pub production_date: String,
    /// Next numeric Bates number after the final page of this volume.
    pub next_bates_number: u64,
    /// Per-document production records in input order.
    pub documents: Vec<ProducedDocumentRecord>,
    /// All generated files (`IMAGES/*`, `TEXT/*`, `NATIVES/*`, `LOADFILES/*`) sorted by `relative_path`.
    pub artifacts: Vec<ProducedArtifact>,
}

/// Scale `page` into a safe inner margin when endorsements are active and draw the 6 header/footer
/// endorsement strings so endorsements **never overlap** document content.
#[must_use]
pub fn apply_endorsements_to_page(
    page: &LayoutPage,
    endorsements: &EndorsementConfig,
    tokens: &EndorsementTokens<'_>,
    fonts: &FontStore,
    font_id: FontId,
) -> LayoutPage {
    let has_header = endorsements.has_header();
    let has_footer = endorsements.has_footer();
    if !has_header && !has_footer {
        return page.clone();
    }

    let w = page.size.width.max(72.0);
    let h = page.size.height.max(72.0);
    let font_size = endorsements.font_size_pt.clamp(6.0, 14.0);
    let band_h = (font_size * 2.4).max(20.0);
    let top_inset = if has_header { band_h } else { 0.0 };
    let bottom_inset = if has_footer { band_h } else { 0.0 };
    let avail_h = (h - top_inset - bottom_inset).max(36.0);
    let scale = (avail_h / h).clamp(0.5, 1.0);
    let scaled_w = w * scale;
    let tx = (w - scaled_w) * 0.5;
    let ty = top_inset;

    let mut out_page = LayoutPage::new(page.index, page.size);
    out_page.label.clone_from(&page.label);

    // Wrap original page items in a scaled/translated group so nothing is ever overlapped
    if !page.items.is_empty() {
        let clip = ClipPath {
            commands: vec![
                PathCommand::MoveTo(Point::new(0.0, 0.0)),
                PathCommand::LineTo(Point::new(w, 0.0)),
                PathCommand::LineTo(Point::new(w, h)),
                PathCommand::LineTo(Point::new(0.0, h)),
                PathCommand::Close,
            ],
            fill_rule: FillRule::NonZero,
        };
        out_page.items.push(Item::Group(GroupItem {
            transform: Affine::translate(tx, ty).then(Affine::scale(scale)),
            clip: Some(clip),
            opacity: 1.0,
            children: page.items.clone(),
        }));
    }

    let margin_x = 18.0_f32;
    let header_baseline_y = font_size + 5.0;
    let footer_baseline_y = h - 6.0;

    // Header Left / Center / Right
    if let Some(ref tpl) = endorsements.header_left {
        let text = tokens.expand(tpl);
        if !text.is_empty() {
            out_page.items.push(Item::Glyphs(build_endorsement_run(
                fonts,
                font_id,
                font_size,
                &text,
                margin_x,
                header_baseline_y,
                EndorseAlign::Left,
                page.index,
            )));
        }
    }
    if let Some(ref tpl) = endorsements.header_center {
        let text = tokens.expand(tpl);
        if !text.is_empty() {
            out_page.items.push(Item::Glyphs(build_endorsement_run(
                fonts,
                font_id,
                font_size,
                &text,
                w * 0.5,
                header_baseline_y,
                EndorseAlign::Center,
                page.index,
            )));
        }
    }
    if let Some(ref tpl) = endorsements.header_right {
        let text = tokens.expand(tpl);
        if !text.is_empty() {
            out_page.items.push(Item::Glyphs(build_endorsement_run(
                fonts,
                font_id,
                font_size,
                &text,
                w - margin_x,
                header_baseline_y,
                EndorseAlign::Right,
                page.index,
            )));
        }
    }

    // Footer Left / Center / Right
    if let Some(ref tpl) = endorsements.footer_left {
        let text = tokens.expand(tpl);
        if !text.is_empty() {
            out_page.items.push(Item::Glyphs(build_endorsement_run(
                fonts,
                font_id,
                font_size,
                &text,
                margin_x,
                footer_baseline_y,
                EndorseAlign::Left,
                page.index,
            )));
        }
    }
    if let Some(ref tpl) = endorsements.footer_center {
        let text = tokens.expand(tpl);
        if !text.is_empty() {
            out_page.items.push(Item::Glyphs(build_endorsement_run(
                fonts,
                font_id,
                font_size,
                &text,
                w * 0.5,
                footer_baseline_y,
                EndorseAlign::Center,
                page.index,
            )));
        }
    }
    if let Some(ref tpl) = endorsements.footer_right {
        let text = tokens.expand(tpl);
        if !text.is_empty() {
            out_page.items.push(Item::Glyphs(build_endorsement_run(
                fonts,
                font_id,
                font_size,
                &text,
                w - margin_x,
                footer_baseline_y,
                EndorseAlign::Right,
                page.index,
            )));
        }
    }

    out_page
}

/// Build a single-page US Letter [`LayoutDocument`] representing a placeholder slip sheet.
#[must_use]
pub fn build_slip_sheet_document(
    spec: &SlipSheetSpec,
    doc_id: &str,
    file_name: &str,
    bates: &str,
    production_date: &str,
) -> LayoutDocument {
    let mut doc = LayoutDocument::new();
    let bold_font = doc.fonts.register_ttf(
        "Liberation Sans",
        700,
        false,
        FontSourceKind::Bundled,
        BUNDLED_SANS_BOLD.to_vec(),
        0,
    );
    let reg_font = doc.fonts.register_ttf(
        "Liberation Sans",
        400,
        false,
        FontSourceKind::Bundled,
        BUNDLED_SANS_REGULAR.to_vec(),
        0,
    );

    let mut page = LayoutPage::new(0, Size::LETTER);

    // Outer border box
    page.items.push(Item::Path(PathItem {
        commands: vec![
            PathCommand::MoveTo(Point::new(54.0, 54.0)),
            PathCommand::LineTo(Point::new(558.0, 54.0)),
            PathCommand::LineTo(Point::new(558.0, 738.0)),
            PathCommand::LineTo(Point::new(54.0, 738.0)),
            PathCommand::Close,
        ],
        fill: None,
        fill_rule: FillRule::NonZero,
        stroke: Some(StrokeStyle {
            color: Rgba::rgb(80, 80, 80),
            width: 1.5,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            dash_array: Vec::new(),
            dash_offset: 0.0,
        }),
        transform: Affine::IDENTITY,
    }));

    // Banner title centered near top
    page.items.push(Item::Glyphs(build_endorsement_run(
        &doc.fonts,
        bold_font,
        15.0,
        &spec.title,
        306.0,
        140.0,
        EndorseAlign::Center,
        0,
    )));

    let reason_str = match &spec.kind {
        SlipSheetKind::NativeOnly => "Produced in Native Format".to_string(),
        SlipSheetKind::Privileged => "Withheld for Privilege".to_string(),
        SlipSheetKind::Withheld => "Withheld".to_string(),
        SlipSheetKind::Failed { error } => format!("Processing Error: {error}"),
    };

    let mut lines = vec![
        format!("Bates Number: {bates}"),
        format!("Document ID:  {doc_id}"),
        format!("File Name:    {file_name}"),
        format!("Status:       {reason_str}"),
        format!("Date:         {production_date}"),
    ];
    for extra in &spec.custom_lines {
        if !extra.trim().is_empty() {
            lines.push(extra.clone());
        }
    }

    let mut y = 210.0_f32;
    for line in lines {
        page.items.push(Item::Glyphs(build_endorsement_run(
            &doc.fonts,
            reg_font,
            11.0,
            &line,
            90.0,
            y,
            EndorseAlign::Left,
            0,
        )));
        y += 22.0;
    }

    doc.pages.push(page);
    doc
}

/// Run a deterministic eDiscovery production for `inputs` into `output_dir`, supporting
/// automatic crash resumption from `output_dir/manifest.json`.
///
/// # Errors
///
/// Returns an [`Error`] if output directories cannot be created, if redaction verification
/// fails on any produced artifact, or if file I/O fails.
pub fn run_production(
    inputs: &[ProductionInput],
    options: &ProductionOptions,
    output_dir: &Path,
) -> Result<ProductionManifest> {
    fs::create_dir_all(output_dir)?;
    let images_dir = output_dir.join("IMAGES");
    let text_dir = output_dir.join("TEXT");
    let natives_dir = output_dir.join("NATIVES");
    let loadfiles_dir = output_dir.join("LOADFILES");
    fs::create_dir_all(&images_dir)?;
    if options.include_text {
        fs::create_dir_all(&text_dir)?;
    }
    fs::create_dir_all(&loadfiles_dir)?;

    let options_sha = options.options_sha256();
    let manifest_path = output_dir.join("manifest.json");

    // Check for an existing manifest for crash resumption
    let existing_manifest: Option<ProductionManifest> = if options.resume && manifest_path.exists()
    {
        fs::read_to_string(&manifest_path)
            .ok()
            .and_then(|s| serde_json::from_str::<ProductionManifest>(&s).ok())
            .filter(|m| {
                m.schema_version == PRODUCTION_MANIFEST_SCHEMA_VERSION
                    && m.engine_version == ENGINE_VERSION
                    && m.options_sha256 == options_sha
            })
    } else {
        None
    };

    let mut current_bates_num = options.bates.start_number;
    let mut records: Vec<ProducedDocumentRecord> = Vec::with_capacity(inputs.len());
    let mut artifacts_map: BTreeMap<String, ProducedArtifact> = BTreeMap::new();
    let mut combined_pdf_doc: Option<LayoutDocument> =
        if matches!(options.pdf_mode, ProductionPdfMode::CombinedVolume { .. }) {
            Some(LayoutDocument::new())
        } else {
            None
        };

    for input in inputs {
        let input_sha256 = sha256_hex(&input.bytes);

        // If resuming and this document was already completed with intact artifacts on disk, reuse it!
        if combined_pdf_doc.is_none()
            && let Some(ref prev_m) = existing_manifest
            && let Some(prev_rec) = prev_m
                .documents
                .iter()
                .find(|d| d.doc_id == input.doc_id && d.sha256 == input_sha256)
            // Artifact names carry the Bates number, so a document is reused only where it
            // still starts at the same number (the inputs before it are unchanged).
            && prev_rec.begin_bates == options.bates.format_bates(current_bates_num)
            && verify_existing_record_artifacts(output_dir, prev_rec, prev_m, &mut artifacts_map)
        {
            #[allow(clippy::cast_possible_truncation)]
            let pages_u64 = prev_rec.page_count as u64;
            current_bates_num = current_bates_num.saturating_add(pages_u64);
            records.push(prev_rec.clone());
            continue;
        }

        let input_md5 = md5_hex(&input.bytes);
        let input_sha1 = sha1_hex(&input.bytes);

        // Determine layout, metadata, redactions, and status
        let (mut doc_layout, mut doc_meta, redacted_strings, is_redacted, doc_status, emit_native) =
            prepare_document_for_production(input, options, current_bates_num)?;

        if doc_layout.pages.is_empty() {
            let bates_str = options.bates.format_bates(current_bates_num);
            doc_layout = build_slip_sheet_document(
                &SlipSheetSpec::native_only(),
                &input.doc_id,
                &input.file_name,
                &bates_str,
                &options.production_date,
            );
        }

        let page_count = doc_layout.pages.len().max(1);
        let begin_bates_num = current_bates_num;
        #[allow(clippy::cast_possible_truncation)]
        let end_bates_num = begin_bates_num + (page_count as u64) - 1;
        current_bates_num = end_bates_num + 1;

        let begin_bates = options.bates.format_bates(begin_bates_num);
        let end_bates = options.bates.format_bates(end_bates_num);
        let confidentiality = input.confidentiality.as_deref().unwrap_or("").to_string();

        // Extract plain text BEFORE endorsements if requested, or with page Bates headers
        let text_rel_path = if options.include_text {
            let mut extracted = String::new();
            for (idx, p) in doc_layout.pages.iter().enumerate() {
                if idx > 0 {
                    if options.page_delimited_text {
                        extracted.push_str("\n\x0C\n");
                    } else {
                        extracted.push('\n');
                    }
                }
                extracted.push_str(&p.text());
            }
            let txt_bytes = extracted.into_bytes();
            if is_redacted {
                verify_redacted_output(&txt_bytes, Some("txt"), &redacted_strings)?;
            }
            let rel = format!("TEXT/{begin_bates}.txt");
            write_and_record_artifact(output_dir, &rel, &txt_bytes, &mut artifacts_map)?;
            Some(rel)
        } else {
            None
        };

        // Apply non-overlapping Bates + confidentiality endorsements to every page
        let endorse_font_id = doc_layout.fonts.register_ttf(
            "Liberation Sans",
            400,
            false,
            FontSourceKind::Bundled,
            BUNDLED_SANS_REGULAR.to_vec(),
            0,
        );
        let mut endorsed_pages = Vec::with_capacity(doc_layout.pages.len());
        for (idx, p) in doc_layout.pages.iter().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let page_bates = options.bates.format_bates(begin_bates_num + (idx as u64));
            let tokens = EndorsementTokens {
                bates: &page_bates,
                doc_id: &input.doc_id,
                page: idx + 1,
                page_count,
                confidentiality: &confidentiality,
                date: &options.production_date,
            };
            let mut ep = apply_endorsements_to_page(
                p,
                &options.endorsements,
                &tokens,
                &doc_layout.fonts,
                endorse_font_id,
            );
            ep.label = Some(page_bates);
            endorsed_pages.push(ep);
        }
        doc_layout.pages = endorsed_pages;

        // Render images (SinglePageTiff, MultiPageTiff, SinglePagePng)
        let mut image_paths = Vec::new();
        let raster_opts = RasterOptions {
            dpi: options.dpi,
            ..RasterOptions::default()
        };

        match options.image_format {
            ProductionImageFormat::None => {}
            ProductionImageFormat::SinglePageTiff => {
                for (idx, page) in doc_layout.pages.iter().enumerate() {
                    #[allow(clippy::cast_possible_truncation)]
                    let page_bates = options.bates.format_bates(begin_bates_num + (idx as u64));
                    let raster = render_page_rgba(
                        page,
                        &doc_layout.fonts,
                        &doc_layout.images,
                        &raster_opts,
                    )?;
                    let tiff_bytes = encode_single_page_production_tiff(&raster)?;
                    if is_redacted {
                        verify_redacted_output(&tiff_bytes, Some("tiff"), &redacted_strings)?;
                    }
                    let rel = format!("IMAGES/{page_bates}.tif");
                    write_and_record_artifact(output_dir, &rel, &tiff_bytes, &mut artifacts_map)?;
                    image_paths.push(rel);
                }
            }
            ProductionImageFormat::MultiPageTiff => {
                let mut cursor = Cursor::new(Vec::new());
                {
                    let mut tiff = TiffEncoder::new(&mut cursor)
                        .map_err(|e| Error::RenderError(format!("TIFF init error: {e}")))?
                        .with_compression(TiffCompression::Deflate(DeflateLevel::Balanced));
                    for page in &doc_layout.pages {
                        let raster = render_page_rgba(
                            page,
                            &doc_layout.fonts,
                            &doc_layout.images,
                            &raster_opts,
                        )?;
                        write_raster_to_tiff(&mut tiff, &raster)?;
                    }
                }
                let tiff_bytes = cursor.into_inner();
                if is_redacted {
                    verify_redacted_output(&tiff_bytes, Some("tiff"), &redacted_strings)?;
                }
                let rel = format!("IMAGES/{begin_bates}.tif");
                write_and_record_artifact(output_dir, &rel, &tiff_bytes, &mut artifacts_map)?;
                image_paths.push(rel);
            }
            ProductionImageFormat::SinglePagePng => {
                for (idx, page) in doc_layout.pages.iter().enumerate() {
                    #[allow(clippy::cast_possible_truncation)]
                    let page_bates = options.bates.format_bates(begin_bates_num + (idx as u64));
                    let png_bytes =
                        render_page_png(page, &doc_layout.fonts, &doc_layout.images, &raster_opts)?;
                    if is_redacted {
                        verify_redacted_output(&png_bytes, Some("png"), &redacted_strings)?;
                    }
                    let rel = format!("IMAGES/{page_bates}.png");
                    write_and_record_artifact(output_dir, &rel, &png_bytes, &mut artifacts_map)?;
                    image_paths.push(rel);
                }
            }
        }

        // Render searchable PDF (PerDocument / CombinedVolume)
        let pdf_rel_path = match options.pdf_mode {
            ProductionPdfMode::None => None,
            ProductionPdfMode::PerDocument { pdf_a2b }
            | ProductionPdfMode::CombinedVolume { pdf_a2b } => {
                let pdf_opts = PdfExportOptions {
                    compress_streams: true,
                    pdf_a2b,
                    layout_options: LayoutOptions::default(),
                };
                let pdf_bytes = export_document_pdf(&doc_layout, &pdf_opts)?;
                if is_redacted {
                    verify_redacted_output(&pdf_bytes, Some("pdf"), &redacted_strings)?;
                }
                let rel = format!("IMAGES/{begin_bates}.pdf");
                write_and_record_artifact(output_dir, &rel, &pdf_bytes, &mut artifacts_map)?;

                if let Some(ref mut combined) = combined_pdf_doc {
                    merge_document_into_combined(combined, &doc_layout);
                }
                Some(rel)
            }
        };

        // Copy native file only when requested and NOT redacted (never ship unredacted natives for redacted docs)
        let native_rel_path = if emit_native && !is_redacted {
            fs::create_dir_all(&natives_dir)?;
            let safe_name: String = input
                .file_name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let rel = format!("NATIVES/{begin_bates}_{safe_name}");
            write_and_record_artifact(output_dir, &rel, &input.bytes, &mut artifacts_map)?;
            Some(rel)
        } else {
            None
        };

        let title = doc_meta.title.take().unwrap_or_default();
        let author = doc_meta.author.take().unwrap_or_default();

        records.push(ProducedDocumentRecord {
            doc_id: input.doc_id.clone(),
            file_name: input.file_name.clone(),
            parent_doc_id: input.parent_doc_id.clone(),
            begin_bates: begin_bates.clone(),
            end_bates: end_bates.clone(),
            begin_attach: begin_bates,
            end_attach: end_bates,
            page_count,
            custodian: input.custodian.clone().unwrap_or_default(),
            confidentiality,
            md5: input_md5,
            sha1: input_sha1,
            sha256: input_sha256,
            title,
            author,
            text_path: text_rel_path,
            native_path: native_rel_path,
            image_paths,
            pdf_path: pdf_rel_path,
            redacted: is_redacted,
            status: doc_status,
            extra_fields: input.extra_fields.clone(),
        });

        // Record progress after every document, so an interrupted run can resume from here.
        write_manifest_atomic(
            &manifest_path,
            &build_manifest(
                options,
                &options_sha,
                &records,
                &artifacts_map,
                current_bates_num,
            ),
        )?;
    }

    // Compute family attachment ranges (`begin_attach` / `end_attach`) across parent-child families
    assign_family_attachment_ranges(&mut records);

    // Emit combined volume PDF if requested
    if let ProductionPdfMode::CombinedVolume { pdf_a2b } = options.pdf_mode
        && let Some(ref combined) = combined_pdf_doc
        && !combined.pages.is_empty()
    {
        let pdf_opts = PdfExportOptions {
            compress_streams: true,
            pdf_a2b,
            layout_options: LayoutOptions::default(),
        };
        let pdf_bytes = export_document_pdf(combined, &pdf_opts)?;
        let rel = format!("IMAGES/{}.pdf", options.volume_name);
        write_and_record_artifact(output_dir, &rel, &pdf_bytes, &mut artifacts_map)?;
    }

    // Write load files (`Concordance .dat`, `Opticon .opt`, `CSV .csv`)
    if options.load_files.concordance_dat {
        let dat_content =
            format_concordance_dat(&records, &options.load_files, &options.production_date);
        let rel = format!("LOADFILES/{}.dat", options.volume_name);
        write_and_record_artifact(output_dir, &rel, dat_content.as_bytes(), &mut artifacts_map)?;
    }
    if options.load_files.opticon_opt {
        let opt_content = format_opticon_opt(&records, &options.volume_name, &options.bates);
        let rel = format!("LOADFILES/{}.opt", options.volume_name);
        write_and_record_artifact(output_dir, &rel, opt_content.as_bytes(), &mut artifacts_map)?;
    }
    if options.load_files.csv {
        let csv_content =
            format_csv_load_file(&records, &options.load_files, &options.production_date);
        let rel = format!("LOADFILES/{}.csv", options.volume_name);
        write_and_record_artifact(output_dir, &rel, csv_content.as_bytes(), &mut artifacts_map)?;
    }

    let manifest = build_manifest(
        options,
        &options_sha,
        &records,
        &artifacts_map,
        current_bates_num,
    );
    write_manifest_atomic(&manifest_path, &manifest)?;
    Ok(manifest)
}

/// Stream a document's produced pages in bounded `batch_size` windows, invoking `on_page`
/// with `(zero_based_page_index, formatted_bates, encoded_image_bytes)` and dropping each
/// batch's `LayoutPage` and raster buffer before advancing to the next batch.
///
/// This images the document as stored: it applies no annotation redactions and writes no
/// manifest or load files, so documents with approved redactions go through
/// [`run_production`]. Each batch parses the document again, trading time for the memory of
/// holding every page; an endorsement that uses `{page_count}` costs one extra full layout to
/// learn the total.
///
/// Returns the total number of pages streamed.
///
/// # Errors
///
/// Returns an [`Error`] if format detection, layout, rasterization, or `on_page` fails.
pub fn export_pages_streaming<F>(
    data: &[u8],
    filename: Option<&str>,
    options: &ProductionOptions,
    batch_size: usize,
    mut on_page: F,
) -> Result<usize>
where
    F: FnMut(usize, String, Vec<u8>) -> Result<()>,
{
    let step = batch_size.max(1);
    let layout_opts = LayoutOptions::default();
    let raster_opts = RasterOptions {
        dpi: options.dpi,
        ..RasterOptions::default()
    };
    let mut start = 0usize;
    let mut total_streamed = 0usize;

    let e = &options.endorsements;
    let uses_page_count = [
        &e.header_left,
        &e.header_center,
        &e.header_right,
        &e.footer_left,
        &e.footer_center,
        &e.footer_right,
    ]
    .into_iter()
    .flatten()
    .any(|t| t.contains("{page_count}"));
    let total_pages = if uses_page_count {
        Some(
            open_and_layout(data, filename, &layout_opts)?
                .layout
                .pages
                .len(),
        )
    } else {
        None
    };

    loop {
        let end = start.saturating_add(step);
        let mut opened = open_and_layout_page_range(data, filename, &layout_opts, start..end)?;
        if opened.layout.pages.is_empty() {
            break;
        }
        let batch_len = opened.layout.pages.len();
        let endorse_font_id = opened.layout.fonts.register_ttf(
            "Liberation Sans",
            400,
            false,
            FontSourceKind::Bundled,
            BUNDLED_SANS_REGULAR.to_vec(),
            0,
        );

        for (i, page) in opened.layout.pages.iter().enumerate() {
            let global_idx = start + i;
            #[allow(clippy::cast_possible_truncation)]
            let bates_num = options.bates.start_number + (global_idx as u64);
            let page_bates = options.bates.format_bates(bates_num);
            let tokens = EndorsementTokens {
                bates: &page_bates,
                doc_id: filename.unwrap_or("STREAM"),
                page: global_idx + 1,
                page_count: total_pages.unwrap_or(global_idx + 1),
                confidentiality: "",
                date: &options.production_date,
            };
            let endorsed = apply_endorsements_to_page(
                page,
                &options.endorsements,
                &tokens,
                &opened.layout.fonts,
                endorse_font_id,
            );
            let img_bytes = if options.image_format == ProductionImageFormat::SinglePagePng {
                render_page_png(
                    &endorsed,
                    &opened.layout.fonts,
                    &opened.layout.images,
                    &raster_opts,
                )?
            } else {
                let raster = render_page_rgba(
                    &endorsed,
                    &opened.layout.fonts,
                    &opened.layout.images,
                    &raster_opts,
                )?;
                encode_single_page_production_tiff(&raster)?
            };
            on_page(global_idx, page_bates, img_bytes)?;
            total_streamed += 1;
        }

        if batch_len < step {
            break;
        }
        start = end;
    }

    Ok(total_streamed)
}

fn build_manifest(
    options: &ProductionOptions,
    options_sha: &str,
    records: &[ProducedDocumentRecord],
    artifacts: &BTreeMap<String, ProducedArtifact>,
    next_bates_number: u64,
) -> ProductionManifest {
    ProductionManifest {
        schema_version: PRODUCTION_MANIFEST_SCHEMA_VERSION,
        volume_name: options.volume_name.clone(),
        engine_version: ENGINE_VERSION.to_string(),
        options_sha256: options_sha.to_string(),
        production_date: options.production_date.clone(),
        next_bates_number,
        documents: records.to_vec(),
        artifacts: artifacts.values().cloned().collect(),
    }
}

/// Write `manifest` to a temporary file and rename it into place, so a crash never leaves a
/// truncated manifest behind.
fn write_manifest_atomic(path: &Path, manifest: &ProductionManifest) -> Result<()> {
    let json = serde_json::to_string_pretty(manifest)
        .map_err(|e| Error::Internal(format!("Manifest serialization failed: {e}")))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Format a Concordance `.dat` load file using the configured field, quote, and newline delimiters.
#[must_use]
pub fn format_concordance_dat(
    records: &[ProducedDocumentRecord],
    config: &LoadFileConfig,
    production_date: &str,
) -> String {
    let q = config.quote_delimiter;
    let d = config.field_delimiter;
    let nl = config.newline_replacement;
    let mut out = String::new();

    // Header row
    for (idx, col) in config.columns.iter().enumerate() {
        if idx > 0 {
            out.push(d);
        }
        out.push(q);
        out.push_str(&sanitize_concordance_val(col, q, d, nl));
        out.push(q);
    }
    out.push('\n');

    // Data rows
    for rec in records {
        for (idx, col) in config.columns.iter().enumerate() {
            if idx > 0 {
                out.push(d);
            }
            let val = resolve_load_file_column(rec, col, production_date);
            out.push(q);
            out.push_str(&sanitize_concordance_val(&val, q, d, nl));
            out.push(q);
        }
        out.push('\n');
    }

    out
}

/// Reference reader for Concordance `.dat` load files, returning rows as `BTreeMap<column_name, value>`.
///
/// # Errors
///
/// Returns [`Error::InvalidInput`] if the header row is missing or a data row has a mismatched column count.
pub fn parse_concordance_dat(
    content: &str,
    field_delimiter: char,
    quote_delimiter: char,
    newline_replacement: char,
) -> Result<Vec<BTreeMap<String, String>>> {
    let mut lines = content.lines().filter(|l| !l.is_empty());
    let header_line = lines
        .next()
        .ok_or_else(|| Error::InvalidInput("Empty Concordance .dat content".to_string()))?;
    let headers = parse_concordance_row(
        header_line,
        field_delimiter,
        quote_delimiter,
        newline_replacement,
    );

    let mut rows = Vec::new();
    for (line_idx, line) in lines.enumerate() {
        let vals =
            parse_concordance_row(line, field_delimiter, quote_delimiter, newline_replacement);
        if vals.len() != headers.len() {
            return Err(Error::InvalidInput(format!(
                "Concordance .dat row {} has {} fields, expected {}",
                line_idx + 1,
                vals.len(),
                headers.len()
            )));
        }
        let mut map = BTreeMap::new();
        for (k, v) in headers.iter().zip(vals) {
            map.insert(k.clone(), v);
        }
        rows.push(map);
    }
    Ok(rows)
}

/// Format an Opticon `.opt` page-level image cross-reference load file.
///
/// Format per line: `BatesNumber,VolumeName,RelativeImagePath,DocBreak(Y or empty),,,PageCount(on first page)`
#[must_use]
pub fn format_opticon_opt(
    records: &[ProducedDocumentRecord],
    volume_name: &str,
    bates_cfg: &BatesConfig,
) -> String {
    let mut out = String::new();
    for rec in records {
        let start_num = bates_cfg.parse_bates_number(&rec.begin_bates).unwrap_or(1);
        for page_idx in 0..rec.page_count {
            #[allow(clippy::cast_possible_truncation)]
            let page_bates = bates_cfg.format_bates(start_num + (page_idx as u64));
            let img_path = rec
                .image_paths
                .get(page_idx)
                .or_else(|| rec.image_paths.first())
                .or(rec.pdf_path.as_ref())
                .cloned()
                .unwrap_or_else(|| format!("IMAGES/{page_bates}.tif"))
                .replace('/', "\\");
            let doc_break = if page_idx == 0 { "Y" } else { "" };
            let page_cnt = if page_idx == 0 {
                rec.page_count.to_string()
            } else {
                String::new()
            };
            let _ = writeln!(
                out,
                "{page_bates},{volume_name},{img_path},{doc_break},,,{page_cnt}"
            );
        }
    }
    out
}

/// Parsed row from an Opticon `.opt` load file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpticonRow {
    /// Page Bates identifier.
    pub bates: String,
    /// Volume identifier.
    pub volume: String,
    /// Image path.
    pub image_path: String,
    /// Whether this page starts a new document (`"Y"`).
    pub is_document_break: bool,
    /// Page count recorded on the document-break line.
    pub page_count: Option<usize>,
}

/// Reference reader for Opticon `.opt` load files.
///
/// # Errors
///
/// Returns [`Error::InvalidInput`] if any non-empty line has fewer than 7 comma-separated fields.
pub fn parse_opticon_opt(content: &str) -> Result<Vec<OpticonRow>> {
    let mut rows = Vec::new();
    for (idx, line) in content.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 7 {
            return Err(Error::InvalidInput(format!(
                "Invalid Opticon .opt line {}: expected 7 fields, got {}",
                idx + 1,
                parts.len()
            )));
        }
        let page_count = if parts[6].trim().is_empty() {
            None
        } else {
            Some(parts[6].trim().parse::<usize>().map_err(|e| {
                Error::InvalidInput(format!(
                    "Invalid Opticon page_count on line {}: {e}",
                    idx + 1
                ))
            })?)
        };
        rows.push(OpticonRow {
            bates: parts[0].to_string(),
            volume: parts[1].to_string(),
            image_path: parts[2].to_string(),
            is_document_break: parts[3].eq_ignore_ascii_case("Y"),
            page_count,
        });
    }
    Ok(rows)
}

/// Format an RFC 4180 CSV metadata load file.
#[must_use]
pub fn format_csv_load_file(
    records: &[ProducedDocumentRecord],
    config: &LoadFileConfig,
    production_date: &str,
) -> String {
    let mut out = String::new();
    for (idx, col) in config.columns.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str(&escape_csv_field(col));
    }
    out.push('\n');

    for rec in records {
        for (idx, col) in config.columns.iter().enumerate() {
            if idx > 0 {
                out.push(',');
            }
            let val = resolve_load_file_column(rec, col, production_date);
            out.push_str(&escape_csv_field(&val));
        }
        out.push('\n');
    }
    out
}

/// Reference reader for CSV load files, returning rows as `BTreeMap<column_name, value>`.
///
/// # Errors
///
/// Returns [`Error::InvalidInput`] if the CSV is empty or has mismatched column counts.
pub fn parse_csv_load_file(content: &str) -> Result<Vec<BTreeMap<String, String>>> {
    let mut lines = content.lines().filter(|l| !l.is_empty());
    let header_line = lines
        .next()
        .ok_or_else(|| Error::InvalidInput("Empty CSV load file".to_string()))?;
    let headers = parse_csv_row(header_line);
    let mut out = Vec::new();
    for (idx, line) in lines.enumerate() {
        let vals = parse_csv_row(line);
        if vals.len() != headers.len() {
            return Err(Error::InvalidInput(format!(
                "CSV row {} has {} fields, expected {}",
                idx + 1,
                vals.len(),
                headers.len()
            )));
        }
        let mut map = BTreeMap::new();
        for (k, v) in headers.iter().zip(vals) {
            map.insert(k.clone(), v);
        }
        out.push(map);
    }
    Ok(out)
}

fn prepare_document_for_production(
    input: &ProductionInput,
    options: &ProductionOptions,
    current_bates_num: u64,
) -> Result<(
    LayoutDocument,
    Metadata,
    Vec<String>,
    bool,
    ProducedDocumentStatus,
    bool,
)> {
    let first_bates = options.bates.format_bates(current_bates_num);

    // 1. Explicit slip sheet override (e.g. Privileged, Withheld, NativeOnly)
    if let Some(ref spec) = input.slip_sheet {
        let doc = build_slip_sheet_document(
            spec,
            &input.doc_id,
            &input.file_name,
            &first_bates,
            &options.production_date,
        );
        // An unredacted native must not stand in for a document marked for redaction.
        let emit_native = (matches!(spec.kind, SlipSheetKind::NativeOnly) || input.include_native)
            && !has_approved_redactions(input);
        let reason = match &spec.kind {
            SlipSheetKind::NativeOnly => "native_only".to_string(),
            SlipSheetKind::Privileged => "privileged".to_string(),
            SlipSheetKind::Withheld => "withheld".to_string(),
            SlipSheetKind::Failed { error } => format!("failed: {error}"),
        };
        return Ok((
            doc,
            Metadata::default(),
            Vec::new(),
            false,
            ProducedDocumentStatus::SlipSheeted { reason },
            emit_native,
        ));
    }

    let layout_opts = LayoutOptions::default();

    // 2. Check if this is an Excel spreadsheet
    let is_excel =
        prism_excel::is_xlsx_zip(&input.bytes) || prism_excel::is_xls_bytes(&input.bytes);
    if is_excel {
        let wb_res = if prism_excel::is_xlsx_zip(&input.bytes) {
            prism_excel::parse_xlsx(&input.bytes, layout_opts.max_decompressed_stream_bytes)
        } else {
            prism_excel::parse_xls(&input.bytes, layout_opts.max_decompressed_stream_bytes)
        };

        if let Ok(mut wb) = wb_res {
            let mut redacted_strings = Vec::new();
            let mut is_redacted = false;
            if let Some(ref sidecar) = input.sidecar {
                let wb_log = apply_workbook_redactions(&mut wb, sidecar);
                if wb_log.total_applied > 0 {
                    is_redacted = true;
                    redacted_strings.extend(wb_log.redacted_strings);
                }
            }

            if options.spreadsheet_mode == SpreadsheetProductionMode::NativeWithSlipSheet
                && !is_redacted
                && !has_approved_redactions(input)
            {
                let spec = SlipSheetSpec::native_only();
                let doc = build_slip_sheet_document(
                    &spec,
                    &input.doc_id,
                    &input.file_name,
                    &first_bates,
                    &options.production_date,
                );
                let meta = Metadata {
                    title: wb.title,
                    author: wb.author,
                    ..Metadata::default()
                };
                return Ok((
                    doc,
                    meta,
                    Vec::new(),
                    false,
                    ProducedDocumentStatus::SlipSheeted {
                        reason: "native_only".to_string(),
                    },
                    true,
                ));
            }

            let layout_res = prism_excel::layout_workbook(&wb, &layout_opts);
            if layout_res.is_err() && is_redacted {
                // The fallback below would lay out the original, unredacted cells.
                return Err(Error::InvalidInput(format!(
                    "Document {} has cell redactions but its workbook could not be laid out",
                    input.doc_id
                )));
            }
            if let Ok(mut layout) = layout_res {
                let mut meta = Metadata {
                    title: wb.title,
                    author: wb.author,
                    ..Metadata::default()
                };
                if let Some(ref sidecar) = input.sidecar {
                    let resolved =
                        sidecar.resolve_against(&input.bytes, ENGINE_VERSION, &layout)?;
                    let page_log =
                        apply_redactions(&mut layout, &sidecar.document_sha256, &resolved)?;
                    if page_log.total_applied > 0 {
                        is_redacted = true;
                        for s in page_log.redacted_strings {
                            if !redacted_strings.contains(&s) {
                                redacted_strings.push(s);
                            }
                        }
                    }
                    scrub_metadata(&mut meta, sidecar, &mut redacted_strings);
                }
                return Ok((
                    layout,
                    meta,
                    redacted_strings,
                    is_redacted,
                    ProducedDocumentStatus::Completed,
                    input.include_native,
                ));
            }
        }
    }

    // Cell-range redactions only apply through the workbook model above.
    if input
        .sidecar
        .as_ref()
        .is_some_and(has_approved_cell_redactions)
    {
        return Err(Error::InvalidInput(format!(
            "Document {} has cell-range redactions but could not be read as a workbook",
            input.doc_id
        )));
    }

    // 3. Parse and lay out general documents (PDF, Word, PowerPoint, UDM fallback)
    match parse_and_layout_document(&input.bytes, Some(&input.file_name), &layout_opts) {
        Ok((mut layout, mut meta)) => {
            let mut redacted_strings = Vec::new();
            let mut is_redacted = false;
            if let Some(ref sidecar) = input.sidecar {
                let resolved = sidecar.resolve_against(&input.bytes, ENGINE_VERSION, &layout)?;
                let log = apply_redactions(&mut layout, &sidecar.document_sha256, &resolved)?;
                if log.total_applied > 0 {
                    is_redacted = true;
                    redacted_strings = log.redacted_strings;
                }
                scrub_metadata(&mut meta, sidecar, &mut redacted_strings);
            }
            Ok((
                layout,
                meta,
                redacted_strings,
                is_redacted,
                ProducedDocumentStatus::Completed,
                input.include_native,
            ))
        }
        Err(err) => {
            // If an input has approved redactions and fails to parse, fail hard rather than risking unredacted output
            if has_approved_redactions(input) {
                return Err(err);
            }
            let err_msg = err.to_string();
            let spec = SlipSheetSpec {
                kind: SlipSheetKind::Failed {
                    error: err_msg.clone(),
                },
                title: "DOCUMENT FAILED TO RENDER — PLACEHOLDER SLIP SHEET".to_string(),
                custom_lines: Vec::new(),
            };
            let doc = build_slip_sheet_document(
                &spec,
                &input.doc_id,
                &input.file_name,
                &first_bates,
                &options.production_date,
            );
            Ok((
                doc,
                Metadata::default(),
                Vec::new(),
                false,
                ProducedDocumentStatus::SlipSheeted {
                    reason: format!("failed: {err_msg}"),
                },
                false,
            ))
        }
    }
}

/// Whether `input` carries an approved (not merely proposed) redaction.
fn has_approved_redactions(input: &ProductionInput) -> bool {
    input.sidecar.as_ref().is_some_and(|s| {
        s.annotations
            .iter()
            .any(|a| !a.proposed && matches!(a.kind, AnnotationKind::Redaction { .. }))
    })
}

fn parse_and_layout_document(
    bytes: &[u8],
    filename: Option<&str>,
    options: &LayoutOptions,
) -> Result<(LayoutDocument, Metadata)> {
    let opened = open_and_layout(bytes, filename, options)?;
    Ok((opened.layout, opened.metadata))
}

#[derive(Clone, Copy)]
enum EndorseAlign {
    Left,
    Center,
    Right,
}

fn build_endorsement_run(
    fonts: &FontStore,
    font_id: FontId,
    font_size: f32,
    text: &str,
    anchor_x: f32,
    baseline_y: f32,
    align: EndorseAlign,
    page_index: u32,
) -> GlyphRun {
    let char_adv = font_size * 0.54;
    let mut glyphs = Vec::new();
    let mut clusters = Vec::new();
    let mut cur_x = 0.0_f32;
    let mut cur_byte = 0u32;

    for (idx, ch) in text.chars().enumerate() {
        let gid = fonts.map_char_to_glyph(font_id, ch);
        glyphs.push(PositionedGlyph {
            glyph_id: gid,
            x: cur_x,
            y: 0.0,
            advance: char_adv,
        });
        #[allow(clippy::cast_possible_truncation)]
        let ch_len = ch.len_utf8() as u32;
        #[allow(clippy::cast_possible_truncation)]
        let g_idx = idx as u32;
        clusters.push(Cluster {
            text_range: cur_byte..(cur_byte + ch_len),
            glyph_range: g_idx..(g_idx + 1),
            x: cur_x,
            advance: char_adv,
        });
        cur_x += char_adv;
        cur_byte += ch_len;
    }

    let start_x = match align {
        EndorseAlign::Left => anchor_x,
        EndorseAlign::Center => anchor_x - cur_x * 0.5,
        EndorseAlign::Right => anchor_x - cur_x,
    };

    GlyphRun {
        font: font_id,
        size: font_size,
        color: Rgba::BLACK,
        transform: Affine::translate(start_x.max(4.0), baseline_y),
        glyphs,
        text: text.to_string(),
        clusters,
        decorations: Decorations::default(),
        source: SourceRef::new(page_index, u32::MAX, u32::MAX),
    }
}

fn is_raster_grayscale(raster: &RasterImage) -> bool {
    raster
        .data
        .chunks_exact(4)
        .all(|px| px[0] == px[1] && px[1] == px[2] && px[3] == 255)
}

fn encode_single_page_production_tiff(raster: &RasterImage) -> Result<Vec<u8>> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut tiff = TiffEncoder::new(&mut cursor)
            .map_err(|e| Error::RenderError(format!("TIFF header error: {e}")))?
            .with_compression(TiffCompression::Deflate(DeflateLevel::Balanced));
        write_raster_to_tiff(&mut tiff, raster)?;
    }
    Ok(cursor.into_inner())
}

fn write_raster_to_tiff<W: std::io::Write + std::io::Seek>(
    tiff: &mut TiffEncoder<W>,
    raster: &RasterImage,
) -> Result<()> {
    if is_raster_grayscale(raster) {
        let gray: Vec<u8> = raster.data.chunks_exact(4).map(|px| px[0]).collect();
        tiff.write_image::<colortype::Gray8>(raster.width, raster.height, &gray)
            .map_err(|e| Error::RenderError(format!("TIFF Gray8 write error: {e}")))?;
    } else {
        tiff.write_image::<colortype::RGBA8>(raster.width, raster.height, &raster.data)
            .map_err(|e| Error::RenderError(format!("TIFF RGBA8 write error: {e}")))?;
    }
    Ok(())
}

fn merge_document_into_combined(combined: &mut LayoutDocument, doc: &LayoutDocument) {
    // Re-register fonts and images in `combined` and remap page items
    let mut font_map = BTreeMap::new();
    for (&old_fid, entry) in &doc.fonts.fonts {
        let new_fid = combined.fonts.register_ttf(
            entry.family.clone(),
            entry.weight,
            entry.italic,
            entry.source,
            entry.data.clone(),
            entry.face_index,
        );
        for (&gid, cmds) in &entry.precomputed_outlines {
            combined
                .fonts
                .insert_precomputed_outline(new_fid, gid, cmds.clone());
        }
        font_map.insert(old_fid, new_fid);
    }

    let mut img_map = BTreeMap::new();
    for (&old_iid, entry) in &doc.images.images {
        if let Ok(new_iid) = combined.images.register(
            entry.mime_type.clone(),
            entry.width,
            entry.height,
            entry.data.clone(),
            &LayoutOptions::default(),
        ) {
            img_map.insert(old_iid, new_iid);
        }
    }

    for page in &doc.pages {
        #[allow(clippy::cast_possible_truncation)]
        let next_idx = combined.pages.len() as u32;
        let mut new_page = page.clone();
        new_page.index = next_idx;
        remap_items_ids(&mut new_page.items, &font_map, &img_map);
        combined.pages.push(new_page);
    }
}

fn remap_items_ids(
    items: &mut [Item],
    font_map: &BTreeMap<FontId, FontId>,
    img_map: &BTreeMap<prism_layout::ImageId, prism_layout::ImageId>,
) {
    for item in items {
        match item {
            Item::Glyphs(run) => {
                if let Some(&nf) = font_map.get(&run.font) {
                    run.font = nf;
                }
            }
            Item::Image(img) => {
                if let Some(&ni) = img_map.get(&img.image) {
                    img.image = ni;
                }
            }
            Item::Group(g) => remap_items_ids(&mut g.children, font_map, img_map),
            _ => {}
        }
    }
}

fn assign_family_attachment_ranges(records: &mut [ProducedDocumentRecord]) {
    // Map each root document to the min begin_bates and max end_bates of itself and its descendants
    let mut parent_map: BTreeMap<String, Option<String>> = BTreeMap::new();
    for rec in records.iter() {
        parent_map.insert(rec.doc_id.clone(), rec.parent_doc_id.clone());
    }

    let find_root = |doc_id: &str| -> String {
        let mut cur = doc_id.to_string();
        let mut visited = 0usize;
        while let Some(Some(p)) = parent_map.get(&cur) {
            if visited > 64 || !parent_map.contains_key(p) {
                break;
            }
            cur.clone_from(p);
            visited += 1;
        }
        cur
    };

    let mut family_bounds: BTreeMap<String, (String, String)> = BTreeMap::new();
    for rec in records.iter() {
        let root_id = find_root(&rec.doc_id);
        family_bounds
            .entry(root_id)
            .and_modify(|(beg, end)| {
                if bates_order_key(&rec.begin_bates) < bates_order_key(beg) {
                    beg.clone_from(&rec.begin_bates);
                }
                if bates_order_key(&rec.end_bates) > bates_order_key(end) {
                    end.clone_from(&rec.end_bates);
                }
            })
            .or_insert_with(|| (rec.begin_bates.clone(), rec.end_bates.clone()));
    }

    for rec in records.iter_mut() {
        let root_id = find_root(&rec.doc_id);
        if let Some((beg, end)) = family_bounds.get(&root_id) {
            rec.begin_attach.clone_from(beg);
            rec.end_attach.clone_from(end);
        }
    }
}

/// Sort key for Bates numbers sharing one prefix: a number that outgrew its zero padding is
/// longer, and must still sort after the shorter ones (`ABC10000` after `ABC9999`).
fn bates_order_key(bates: &str) -> (usize, &str) {
    (bates.len(), bates)
}

fn verify_existing_record_artifacts(
    output_dir: &Path,
    rec: &ProducedDocumentRecord,
    manifest: &ProductionManifest,
    artifacts_map: &mut BTreeMap<String, ProducedArtifact>,
) -> bool {
    let mut required_rels: Vec<&str> = Vec::new();
    for img in &rec.image_paths {
        required_rels.push(img.as_str());
    }
    if let Some(ref p) = rec.pdf_path {
        required_rels.push(p.as_str());
    }
    if let Some(ref t) = rec.text_path {
        required_rels.push(t.as_str());
    }
    if let Some(ref n) = rec.native_path {
        required_rels.push(n.as_str());
    }

    let mut verified = Vec::new();
    for rel in required_rels {
        let Some(art) = manifest.artifacts.iter().find(|a| a.relative_path == rel) else {
            return false;
        };
        let full_path = output_dir.join(rel);
        let Ok(bytes) = fs::read(&full_path) else {
            return false;
        };
        if sha256_hex(&bytes) != art.sha256 {
            return false;
        }
        verified.push(art.clone());
    }

    for art in verified {
        artifacts_map.insert(art.relative_path.clone(), art);
    }
    true
}

fn write_and_record_artifact(
    output_dir: &Path,
    relative_path: &str,
    bytes: &[u8],
    artifacts_map: &mut BTreeMap<String, ProducedArtifact>,
) -> Result<()> {
    let full_path = output_dir.join(relative_path);
    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&full_path, bytes)?;
    artifacts_map.insert(
        relative_path.to_string(),
        ProducedArtifact {
            relative_path: relative_path.to_string(),
            sha256: sha256_hex(bytes),
            size_bytes: bytes.len() as u64,
        },
    );
    Ok(())
}

fn resolve_load_file_column(
    rec: &ProducedDocumentRecord,
    col: &str,
    production_date: &str,
) -> String {
    match col.to_ascii_uppercase().as_str() {
        "DOCID" | "CONTROLNUMBER" => rec.doc_id.clone(),
        "BEGBATES" | "BEGINBATES" | "PRODBEG" => rec.begin_bates.clone(),
        "ENDBATES" | "PRODENDBATES" | "PRODEND" => rec.end_bates.clone(),
        "BEGATTACH" | "BEGINATTACH" | "PRODBEGATTACH" => rec.begin_attach.clone(),
        "ENDATTACH" | "PRODENDATTACH" => rec.end_attach.clone(),
        "PAGECOUNT" | "PGCOUNT" => rec.page_count.to_string(),
        "CUSTODIAN" => rec.custodian.clone(),
        "FILENAME" => rec.file_name.clone(),
        "CONFIDENTIALITY" => rec.confidentiality.clone(),
        "MD5" | "MD5HASH" => rec.md5.clone(),
        "SHA1" | "SHA1HASH" => rec.sha1.clone(),
        "SHA256" | "SHA256HASH" => rec.sha256.clone(),
        "TITLE" => rec.title.clone(),
        "AUTHOR" => rec.author.clone(),
        "DATE" | "PRODDATE" => production_date.to_string(),
        "TEXTPATH" | "EXTRACTEDTEXT" => rec.text_path.clone().unwrap_or_default(),
        "NATIVEPATH" | "NATIVELINK" => rec.native_path.clone().unwrap_or_default(),
        "REDACTED" => if rec.redacted { "YES" } else { "NO" }.to_string(),
        other => rec
            .extra_fields
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(other))
            .map(|(_, v)| v.clone())
            .unwrap_or_default(),
    }
}

fn sanitize_concordance_val(val: &str, q: char, d: char, nl: char) -> String {
    val.chars()
        .map(|c| {
            if c == '\n' || c == '\r' {
                nl
            } else if c == q || c == d {
                ' '
            } else {
                c
            }
        })
        .collect()
}

fn parse_concordance_row(line: &str, d: char, q: char, nl: char) -> Vec<String> {
    line.split(d)
        .map(|part| {
            let trimmed = part.trim_matches(q);
            trimmed
                .chars()
                .map(|c| if c == nl { '\n' } else { c })
                .collect()
        })
        .collect()
}

fn escape_csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn parse_csv_row(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    current.push('"');
                    let _ = chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                current.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == ',' {
            fields.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
    }
    fields.push(current);
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_core::review::ReviewRect;
    use prism_review::{Annotation, AnnotationAnchor, AnnotationKind, RedactionTarget};

    #[test]
    fn bates_numbering_and_endorsement_tokens() {
        let cfg = BatesConfig {
            prefix: "ACME_".to_string(),
            start_number: 42,
            padding: 6,
            suffix: "_PROD".to_string(),
        };
        let formatted = cfg.format_bates(42);
        assert_eq!(formatted, "ACME_000042_PROD");
        assert_eq!(cfg.parse_bates_number(&formatted), Some(42));

        let tokens = EndorsementTokens {
            bates: &formatted,
            doc_id: "DOC-99",
            page: 2,
            page_count: 5,
            confidentiality: "HIGHLY CONFIDENTIAL",
            date: "2026-03-01",
        };
        assert_eq!(
            tokens.expand("{bates} | Page {page} of {page_count} | {confidentiality} | {date}"),
            "ACME_000042_PROD | Page 2 of 5 | HIGHLY CONFIDENTIAL | 2026-03-01"
        );
    }

    #[test]
    fn production_determinism_crash_resumption_redaction_and_load_files() {
        let dir_a = std::env::temp_dir().join("prism_prod_test_a");
        let dir_b = std::env::temp_dir().join("prism_prod_test_b");
        let _ = fs::remove_dir_all(&dir_a);
        let _ = fs::remove_dir_all(&dir_b);

        let doc1_bytes =
            b"Parent email body with secret token TOPSECRET-7788 and attachment.".to_vec();
        let doc2_bytes = b"Child attachment text on page one.".to_vec();

        // Build a redaction sidecar for doc1 redacting TOPSECRET-7788
        let (layout1, _) =
            parse_and_layout_document(&doc1_bytes, Some("email.txt"), &LayoutOptions::default())
                .expect("layout1");
        let (ta, rects) = prism_review::TextAnchor::from_page_quote(&layout1, 0, "TOPSECRET-7788")
            .expect("quote");
        let mut sidecar1 = AnnotationSidecar::new(&doc1_bytes);
        sidecar1.add(Annotation {
            id: "red-1".to_string(),
            kind: AnnotationKind::Redaction {
                reason_code: "PRIV".to_string(),
                label: "REDACTED".to_string(),
                fill_color: None,
            },
            anchor: AnnotationAnchor {
                target: RedactionTarget::TextRange,
                page_index: 0,
                rects_pt: rects
                    .into_iter()
                    .map(|r| ReviewRect::new(r.x, r.y, r.width, r.height))
                    .collect(),
                text_anchor: Some(ta),
            },
            author: None,
            created_at: None,
            proposed: false,
        });

        let mut in1 = ProductionInput::new("DOC-001", "email.txt", doc1_bytes);
        in1.custodian = Some("Alice Smith".to_string());
        in1.confidentiality = Some("CONFIDENTIAL".to_string());
        in1.sidecar = Some(sidecar1);

        let mut in2 = ProductionInput::new("DOC-002", "attachment.txt", doc2_bytes);
        in2.parent_doc_id = Some("DOC-001".to_string());
        in2.custodian = Some("Alice Smith".to_string());

        let mut in3 =
            ProductionInput::new("DOC-003", "privileged.txt", b"Privileged memo".to_vec());
        in3.slip_sheet = Some(SlipSheetSpec::privileged("Attorney-Client Privilege"));

        let inputs = vec![in1, in2, in3];
        let options = ProductionOptions {
            volume_name: "VOL001".to_string(),
            bates: BatesConfig {
                prefix: "PRISM".to_string(),
                start_number: 1,
                padding: 7,
                suffix: String::new(),
            },
            image_format: ProductionImageFormat::SinglePageTiff,
            pdf_mode: ProductionPdfMode::PerDocument { pdf_a2b: true },
            dpi: 100.0,
            production_date: "2026-02-01".to_string(),
            ..ProductionOptions::default()
        };

        // Run 1 into dir_a
        let manifest_a = run_production(&inputs, &options, &dir_a).expect("run_production A");
        assert_eq!(manifest_a.documents.len(), 3);
        assert_eq!(manifest_a.documents[0].begin_bates, "PRISM0000001");
        assert_eq!(manifest_a.documents[0].begin_attach, "PRISM0000001");
        assert_eq!(manifest_a.documents[0].end_attach, "PRISM0000002");
        assert_eq!(manifest_a.documents[1].begin_bates, "PRISM0000002");
        assert_eq!(manifest_a.documents[1].begin_attach, "PRISM0000001");
        assert_eq!(manifest_a.documents[1].end_attach, "PRISM0000002");
        assert_eq!(manifest_a.documents[2].begin_bates, "PRISM0000003");
        assert!(manifest_a.documents[0].redacted);

        // Run 2 into dir_b and verify byte-for-byte determinism across all artifacts and manifest
        let manifest_b = run_production(&inputs, &options, &dir_b).expect("run_production B");
        assert_eq!(manifest_a, manifest_b);

        // Simulate a partial crash in dir_b by deleting DOC-002's TIFF image and re-running with resume=true
        let _ = fs::remove_file(dir_b.join("IMAGES/PRISM0000002.tif"));
        let manifest_resumed =
            run_production(&inputs, &options, &dir_b).expect("run_production resumed");
        assert_eq!(manifest_a, manifest_resumed);

        // Parse and validate generated Concordance .dat, Opticon .opt, and CSV load files
        let dat_str =
            fs::read_to_string(dir_a.join("LOADFILES/VOL001.dat")).expect("read VOL001.dat");
        let dat_rows = parse_concordance_dat(
            &dat_str,
            options.load_files.field_delimiter,
            options.load_files.quote_delimiter,
            options.load_files.newline_replacement,
        )
        .expect("parse .dat");
        assert_eq!(dat_rows.len(), 3);
        assert_eq!(
            dat_rows[0].get("BEGBATES").map(String::as_str),
            Some("PRISM0000001")
        );
        assert_eq!(
            dat_rows[0].get("ENDATTACH").map(String::as_str),
            Some("PRISM0000002")
        );
        assert_eq!(dat_rows[0].get("REDACTED").map(String::as_str), Some("YES"));

        let opt_str =
            fs::read_to_string(dir_a.join("LOADFILES/VOL001.opt")).expect("read VOL001.opt");
        let opt_rows = parse_opticon_opt(&opt_str).expect("parse .opt");
        assert_eq!(opt_rows.len(), 3);
        assert!(opt_rows[0].is_document_break);
        assert_eq!(opt_rows[0].bates, "PRISM0000001");

        let csv_str =
            fs::read_to_string(dir_a.join("LOADFILES/VOL001.csv")).expect("read VOL001.csv");
        let csv_rows = parse_csv_load_file(&csv_str).expect("parse .csv");
        assert_eq!(csv_rows.len(), 3);
        assert_eq!(
            csv_rows[1].get("DOCID").map(String::as_str),
            Some("DOC-002")
        );

        let _ = fs::remove_dir_all(&dir_a);
        let _ = fs::remove_dir_all(&dir_b);
    }
}

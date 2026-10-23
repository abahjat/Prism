// SPDX-License-Identifier: AGPL-3.0-only
//! Sparse `Excel` workbook model (`Workbook`) shared by the `.xlsx` and BIFF8 `.xls` readers.
//!
//! Worksheets store cells in a sparse `BTreeMap<(u32, u32), Cell>` keyed by `(row_0based, col_0based)`
//! and column widths in compressed [`ColumnSpan`]s so a `1,048,576 × 16,384` sheet with only a few
//! populated cells consumes kilobytes rather than gigabytes.

use prism_core::document::ImageResource;
use prism_drawingml::{ChartSpace, DrawingTheme};
use prism_layout::model::Rgba;
use prism_numfmt::DateSystem;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Visibility state of a worksheet (`Visible`, `Hidden`, `VeryHidden`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SheetVisibility {
    /// Normal visible tab (`state="visible"` or `hsState == 0x00`).
    #[default]
    Visible,
    /// User-hidden tab (`state="hidden"` or `hsState == 0x01`).
    Hidden,
    /// VBA-level hidden tab (`state="veryHidden"` or `hsState == 0x02`).
    VeryHidden,
}

impl SheetVisibility {
    /// Return the lowercase string token (`"visible"`, `"hidden"`, `"veryHidden"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Visible => "visible",
            Self::Hidden => "hidden",
            Self::VeryHidden => "veryHidden",
        }
    }
}

/// Zero-based inclusive rectangular cell range (`r1..=r2`, `c1..=c2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CellRange {
    /// Zero-based start row.
    pub start_row: u32,
    /// Zero-based start column.
    pub start_col: u32,
    /// Zero-based end row (inclusive).
    pub end_row: u32,
    /// Zero-based end column (inclusive).
    pub end_col: u32,
}

impl CellRange {
    /// Check whether `(row, col)` lies inside this range.
    #[must_use]
    pub const fn contains(&self, row: u32, col: u32) -> bool {
        row >= self.start_row && row <= self.end_row && col >= self.start_col && col <= self.end_col
    }
}

/// Parse an A1 cell reference (`"B12"`, `"$A$1"`) into zero-based `(row, col)`.
#[must_use]
pub fn parse_a1_cell_ref(s: &str) -> Option<(u32, u32)> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    let clean = trimmed.rsplit('!').next().unwrap_or(trimmed);
    let mut col: u32 = 0;
    let mut row_str = String::new();
    let mut saw_col = false;

    for ch in clean.chars() {
        if ch == '$' {
            continue;
        }
        if ch.is_ascii_alphabetic() && row_str.is_empty() {
            saw_col = true;
            let digit = u32::from(ch.to_ascii_uppercase() as u8 - b'A' + 1);
            col = col.checked_mul(26)?.checked_add(digit)?;
        } else if ch.is_ascii_digit() {
            row_str.push(ch);
        } else {
            return None;
        }
    }

    if !saw_col || row_str.is_empty() {
        return None;
    }
    let row_1based = row_str.parse::<u32>().ok()?;
    if row_1based == 0 || col == 0 {
        return None;
    }
    Some((row_1based - 1, col - 1))
}

/// Parse an A1 range reference (`"A1:D20"`, `"Sheet1!$A$1:$F$10"`, or single cell `"C5"`) into [`CellRange`].
#[must_use]
pub fn parse_a1_range(s: &str) -> Option<CellRange> {
    let clean = s.trim().rsplit('!').next().unwrap_or(s.trim());
    if let Some((a, b)) = clean.split_once(':') {
        let (r1, c1) = parse_a1_cell_ref(a)?;
        let (r2, c2) = parse_a1_cell_ref(b)?;
        Some(CellRange {
            start_row: r1.min(r2),
            start_col: c1.min(c2),
            end_row: r1.max(r2),
            end_col: c1.max(c2),
        })
    } else {
        let (r, c) = parse_a1_cell_ref(clean)?;
        Some(CellRange {
            start_row: r,
            start_col: c,
            end_row: r,
            end_col: c,
        })
    }
}

/// Convert zero-based column index (`0` -> `"A"`, `25` -> `"Z"`, `26` -> `"AA"`) to A1 column letters.
#[must_use]
pub fn col_index_to_letters(mut col: u32) -> String {
    let mut chars = Vec::new();
    loop {
        #[allow(clippy::cast_possible_truncation)]
        let rem = (col % 26) as u8;
        chars.push(char::from(b'A' + rem));
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    chars.into_iter().rev().collect()
}

/// Rich-text run inside a shared string or inline rich string (`r`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct RichTextRun {
    /// Text content.
    pub text: String,
    /// Font family override.
    pub font_family: Option<String>,
    /// Font size in points.
    pub font_size_pt: Option<f32>,
    /// Bold flag.
    pub bold: bool,
    /// Italic flag.
    pub italic: bool,
    /// Underline flag.
    pub underline: bool,
    /// Strikethrough flag.
    pub strikethrough: bool,
    /// Run color.
    pub color: Option<Rgba>,
}

/// Stored (or cached) value of a worksheet cell.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum CellValue {
    /// Blank cell (may still carry styling or a formula).
    #[default]
    Empty,
    /// IEEE 754 floating-point number or date serial.
    Number(f64),
    /// Plain UTF-8 string.
    Text(String),
    /// Rich text with per-run styling.
    RichText(Vec<RichTextRun>),
    /// Boolean (`TRUE` / `FALSE`).
    Bool(bool),
    /// Excel error literal (`#DIV/0!`, `#N/A`, `#VALUE!`, `#REF!`, `#NAME?`, `#NUM!`, `#NULL!`).
    Error(String),
    /// ISO-8601 date string (`t="d"`).
    IsoDate(String),
}

impl CellValue {
    /// Short type code for the C ABI (`"n"`, `"s"`, `"b"`, `"e"`, `"d"`, `""`).
    #[must_use]
    pub const fn type_code(&self) -> &'static str {
        match self {
            Self::Empty => "",
            Self::Number(_) => "n",
            Self::Text(_) | Self::RichText(_) => "s",
            Self::Bool(_) => "b",
            Self::Error(_) => "e",
            Self::IsoDate(_) => "d",
        }
    }

    /// Plain text representation of the raw stored value.
    #[must_use]
    pub fn raw_string(&self) -> String {
        match self {
            Self::Empty => String::new(),
            Self::Number(n) => {
                if n.fract() == 0.0 && n.abs() < 1e15 {
                    #[allow(clippy::cast_possible_truncation)]
                    let i = *n as i64;
                    i.to_string()
                } else {
                    n.to_string()
                }
            }
            Self::Text(s) | Self::Error(s) | Self::IsoDate(s) => s.clone(),
            Self::RichText(runs) => runs.iter().map(|r| r.text.as_str()).collect(),
            Self::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        }
    }

    /// Numeric value if `Self::Number(n)`.
    #[must_use]
    pub const fn as_f64(&self) -> Option<f64> {
        if let Self::Number(n) = *self {
            Some(n)
        } else {
            None
        }
    }
}

/// Formula metadata attached to a cell (`<f>` or BIFF8 `FORMULA` record).
///
/// Prism **never** evaluates or recalculates formulas; the cell's [`CellValue`] holds the
/// cached result stored by Excel when the file was last saved.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormulaInfo {
    /// Formula expression text (without leading `'='`).
    pub expression: String,
    /// Whether this is an array (`CSE` / dynamic array) formula (`t="array"`).
    pub is_array: bool,
    /// Array/shared range reference (`ref="C2:C10"`).
    pub ref_range: Option<String>,
    /// Whether the file contained a cached `<v>` value alongside the formula.
    pub has_cached_value: bool,
}

/// A populated worksheet cell.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    /// Stored value (or cached formula result).
    pub value: CellValue,
    /// Optional formula metadata (never recalculated).
    pub formula: Option<FormulaInfo>,
    /// Index into [`WorkbookStyles::cell_xfs`].
    pub style_idx: u32,
    /// Display string formatted by `prism-numfmt`.
    pub display_text: String,
    /// Resolved number format code string (e.g. `"General"`, `"$#,##0.00"`, `"yyyy-mm-dd"`).
    pub num_fmt_code: String,
    /// Optional conditional color override from the number format (e.g. `[Red]`).
    pub display_color: Option<Rgba>,
    /// Whether the cell value is visually concealed (`;;;` format or font color == fill color).
    pub is_concealed: bool,
    /// Optional inert hyperlink target.
    pub hyperlink: Option<String>,
}

/// Compressed column span (`<col min=".." max=".." width=".." hidden=".."/>` or `COLINFO`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnSpan {
    /// Zero-based start column (inclusive).
    pub min_col: u32,
    /// Zero-based end column (inclusive).
    pub max_col: u32,
    /// Column width in points.
    pub width_pt: f32,
    /// Whether columns in this span are hidden (`hidden="1"` or `width == 0`).
    pub hidden: bool,
    /// Default style index for cells in this column span.
    pub style_idx: u32,
    /// Outline group level (`0..=7`).
    pub outline_level: u8,
}

/// Row-level properties (`<row r=".." ht=".." hidden=".."/>` or BIFF8 `ROW`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowProps {
    /// Row height in points.
    pub height_pt: f32,
    /// Whether a custom height was explicitly set (`customHeight="1"`).
    pub custom_height: bool,
    /// Whether the row is hidden (`hidden="1"` or `height == 0`).
    pub hidden: bool,
    /// Default style index for the row.
    pub style_idx: Option<u32>,
    /// Outline group level (`0..=7`).
    pub outline_level: u8,
}

/// Frozen or split pane configuration (`<pane>` or BIFF8 `PANE`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneInfo {
    /// Number of frozen rows at the top (`ySplit`).
    pub frozen_rows: u32,
    /// Number of frozen columns at the left (`xSplit`).
    pub frozen_cols: u32,
    /// Top-left cell in the bottom-right scrollable pane (`topLeftCell`).
    pub top_left_cell: Option<String>,
}

/// Cell comment or threaded comment note.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetComment {
    /// Comment author.
    pub author: String,
    /// Comment body text.
    pub text: String,
    /// Optional threaded replies (`author: text`).
    pub replies: Vec<String>,
}

/// Structured Excel table (`xl/tables/table*.xml`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetTable {
    /// Internal table name.
    pub name: String,
    /// Display name.
    pub display_name: String,
    /// Table range.
    pub range: CellRange,
    /// Column header names.
    pub columns: Vec<String>,
    /// Table style name (e.g. `"TableStyleMedium2"`).
    pub style_name: Option<String>,
    /// Whether autofilter dropdowns are active on header cells.
    pub has_autofilter: bool,
}

/// Differential style (`<dxf>`) applied by a conditional formatting rule.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DxfStyle {
    /// Font color override.
    pub font_color: Option<Rgba>,
    /// Bold override.
    pub bold: Option<bool>,
    /// Italic override.
    pub italic: Option<bool>,
    /// Background fill color override.
    pub fill_color: Option<Rgba>,
    /// Custom number format code override.
    pub num_fmt_code: Option<String>,
}

/// Comparison operator for `type="cellIs"` conditional formatting rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CondOperator {
    /// `lessThan`
    LessThan,
    /// `lessThanOrEqual`
    LessThanOrEqual,
    /// `equal`
    Equal,
    /// `notEqual`
    NotEqual,
    /// `greaterThanOrEqual`
    GreaterThanOrEqual,
    /// `greaterThan`
    GreaterThan,
    /// `between`
    Between,
    /// `notBetween`
    NotBetween,
}

/// Conditional formatting rule kind (`<cfRule>`).
///
/// Per ADR-0009, rules are evaluated strictly against stored/cached cell values;
/// `Expression` formula rules are recorded for forensic inspection and never evaluated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConditionalRuleKind {
    /// `type="cellIs"` comparing the stored value against 1 or 2 literal/cell operands.
    CellIs {
        /// Comparison operator.
        operator: CondOperator,
        /// First formula/literal operand.
        formula1: String,
        /// Second formula/literal operand (for `Between` / `NotBetween`).
        formula2: Option<String>,
    },
    /// `type="containsText"` / `notContainsText` / `beginsWith` / `endsWith`.
    TextMatch {
        /// Match mode (`"containsText"`, `"notContainsText"`, `"beginsWith"`, `"endsWith"`).
        mode: String,
        /// Needle string.
        text: String,
    },
    /// `type="containsBlanks"` or `type="notContainsBlanks"`.
    BlankCheck {
        /// `true` for `containsBlanks`, `false` for `notContainsBlanks`.
        is_blank: bool,
    },
    /// `type="containsErrors"` or `type="notContainsErrors"`.
    ErrorCheck {
        /// `true` for `containsErrors`, `false` for `notContainsErrors`.
        is_error: bool,
    },
    /// `type="duplicateValues"` or `type="uniqueValues"`.
    DuplicateUnique {
        /// `true` for `duplicateValues`, `false` for `uniqueValues`.
        duplicate: bool,
    },
    /// `type="top10"` (top/bottom N or N%).
    Top10 {
        /// Rank count `N`.
        rank: u32,
        /// Whether `rank` is a percentage.
        percent: bool,
        /// `true` for bottom N, `false` for top N.
        bottom: bool,
    },
    /// `type="aboveAverage"` (above/below range mean).
    AboveAverage {
        /// `true` for above average, `false` for below average.
        above: bool,
        /// Whether equal to average is included.
        equal_average: bool,
    },
    /// `type="colorScale"` (2-color or 3-color gradient).
    ColorScale {
        /// Minimum color.
        min_color: Rgba,
        /// Optional midpoint color.
        mid_color: Option<Rgba>,
        /// Maximum color.
        max_color: Rgba,
    },
    /// `type="dataBar"` (proportional horizontal bar inside the cell).
    DataBar {
        /// Bar fill color.
        color: Rgba,
        /// Whether to show the cell text alongside the bar.
        show_value: bool,
    },
    /// `type="iconSet"` (3–5 directional or status icons).
    IconSet {
        /// Icon set name (`"3Arrows"`, `"3TrafficLights1"`, etc.).
        icon_set: String,
        /// Reverse icon order.
        reverse: bool,
        /// Show value alongside icon.
        show_value: bool,
    },
    /// `type="expression"` — recorded for forensic inspection, never evaluated.
    Expression {
        /// Formula expression.
        formula: String,
    },
}

/// A conditional formatting rule applied to one or more [`CellRange`]s (`sqref`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionalFormatRule {
    /// Target cell ranges (`@sqref`).
    pub ranges: Vec<CellRange>,
    /// Rule evaluation priority (`1` = highest priority).
    pub priority: u32,
    /// Stop evaluating lower-priority rules on match (`@stopIfTrue="1"`).
    pub stop_if_true: bool,
    /// Differential style index (`@dxfId`).
    pub dxf_id: Option<u32>,
    /// Rule kind.
    pub kind: ConditionalRuleKind,
}

/// Anchor coordinates for a drawing object on a worksheet (`xdr:twoCellAnchor`, `xdr:oneCellAnchor`, `xdr:absoluteAnchor`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DrawingAnchor {
    /// Start cell `(row, col)` (0-based).
    pub from_row: u32,
    /// Start column (0-based).
    pub from_col: u32,
    /// Offset from start column left edge in points.
    pub from_col_off_pt: f32,
    /// Offset from start row top edge in points.
    pub from_row_off_pt: f32,
    /// Optional end cell `(row, col)` (for `twoCellAnchor`).
    pub to_row: Option<u32>,
    /// Optional end column (for `twoCellAnchor`).
    pub to_col: Option<u32>,
    /// Offset in end column in points.
    pub to_col_off_pt: f32,
    /// Offset in end row in points.
    pub to_row_off_pt: f32,
    /// Explicit width in points (for `oneCellAnchor` / `absoluteAnchor` or fallback).
    pub width_pt: f32,
    /// Explicit height in points (for `oneCellAnchor` / `absoluteAnchor` or fallback).
    pub height_pt: f32,
}

/// Payload of an [`AnchoredDrawing`] on a worksheet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WorksheetDrawingKind {
    /// Embedded raster or vector image (`xdr:pic`).
    Image {
        /// Image resource ID in [`Workbook::images`].
        resource_id: String,
    },
    /// Native `DrawingML` chart (`c:chartSpace` or `cx:chartSpace`).
    Chart(Box<ChartSpace>),
    /// Shape or text box (`xdr:sp`).
    Shape {
        /// Preset geometry name (`"rect"`, `"roundRect"`, etc.).
        preset_geom: String,
        /// Optional text content inside the shape.
        text: String,
        /// Fill color.
        fill_color: Option<Rgba>,
        /// Stroke color.
        stroke_color: Option<Rgba>,
    },
}

/// A drawing object (chart, image, or shape) anchored to a worksheet grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnchoredDrawing {
    /// Unique object ID within the sheet (`"d0"`, `"d1"`, ...).
    pub id: String,
    /// Object name.
    pub name: String,
    /// Anchor coordinates.
    pub anchor: DrawingAnchor,
    /// Drawing payload.
    pub kind: WorksheetDrawingKind,
}

/// Page orientation for worksheet print layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PageOrientation {
    /// Portrait (`8.5" × 11"`).
    #[default]
    Portrait,
    /// Landscape (`11" × 8.5"`).
    Landscape,
}

/// Worksheet print setup (`<pageSetup>`, `<pageMargins>`, `<printOptions>`, `<headerFooter>`, print area/titles).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct PrintSetup {
    /// Page orientation.
    pub orientation: PageOrientation,
    /// Page width in points (before orientation swap; default `612.0` Letter).
    pub paper_width_pt: f32,
    /// Page height in points (before orientation swap; default `792.0` Letter).
    pub paper_height_pt: f32,
    /// Explicit scaling percentage (`10..=400`, default `100`).
    pub scale_pct: u16,
    /// Fit to N pages wide (`fitToWidth`), if fit-to-page is enabled.
    pub fit_to_width: Option<u16>,
    /// Fit to N pages tall (`fitToHeight`), if fit-to-page is enabled.
    pub fit_to_height: Option<u16>,
    /// Left margin in points (default `50.4 pt` = `0.7"`).
    pub margin_left_pt: f32,
    /// Right margin in points (default `50.4 pt` = `0.7"`).
    pub margin_right_pt: f32,
    /// Top margin in points (default `54.0 pt` = `0.75"`).
    pub margin_top_pt: f32,
    /// Bottom margin in points (default `54.0 pt` = `0.75"`).
    pub margin_bottom_pt: f32,
    /// Header margin in points (default `21.6 pt` = `0.3"`).
    pub margin_header_pt: f32,
    /// Footer margin in points (default `21.6 pt` = `0.3"`).
    pub margin_footer_pt: f32,
    /// Print gridlines (`<printOptions gridLines="1"/>`).
    pub print_gridlines: bool,
    /// Print row and column headings (`<printOptions headings="1"/>`).
    pub print_headings: bool,
    /// Optional explicit print area (`_xlnm.Print_Area`).
    pub print_area: Option<CellRange>,
    /// Optional repeating title rows (`start_row..=end_row` from `_xlnm.Print_Titles`).
    pub repeat_rows: Option<(u32, u32)>,
    /// Optional repeating title columns (`start_col..=end_col` from `_xlnm.Print_Titles`).
    pub repeat_cols: Option<(u32, u32)>,
    /// Odd header raw Excel format string (`&L...&C...&R...`).
    pub header_text: Option<String>,
    /// Odd footer raw Excel format string (`&L...&C...&R...`).
    pub footer_text: Option<String>,
    /// Manual horizontal page break row indices (0-based row after which a break occurs).
    pub row_breaks: Vec<u32>,
    /// Manual vertical page break column indices (0-based col after which a break occurs).
    pub col_breaks: Vec<u32>,
}

impl Default for PrintSetup {
    fn default() -> Self {
        Self {
            orientation: PageOrientation::Portrait,
            paper_width_pt: 612.0,
            paper_height_pt: 792.0,
            scale_pct: 100,
            fit_to_width: None,
            fit_to_height: None,
            margin_left_pt: 50.4,
            margin_right_pt: 50.4,
            margin_top_pt: 54.0,
            margin_bottom_pt: 54.0,
            margin_header_pt: 21.6,
            margin_footer_pt: 21.6,
            print_gridlines: true,
            print_headings: false,
            print_area: None,
            repeat_rows: None,
            repeat_cols: None,
            header_text: None,
            footer_text: None,
            row_breaks: Vec::new(),
            col_breaks: Vec::new(),
        }
    }
}

/// Font definition in [`WorkbookStyles::fonts`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ExcelFont {
    /// Font family name (default `"Calibri"`).
    pub name: String,
    /// Font size in points (default `11.0`).
    pub size_pt: f32,
    /// Bold flag.
    pub bold: bool,
    /// Italic flag.
    pub italic: bool,
    /// Underline flag.
    pub underline: bool,
    /// Strikethrough flag.
    pub strikethrough: bool,
    /// Font color.
    pub color: Option<Rgba>,
}

impl Default for ExcelFont {
    fn default() -> Self {
        Self {
            name: "Calibri".to_string(),
            size_pt: 11.0,
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            color: None,
        }
    }
}

/// Fill definition in [`WorkbookStyles::fills`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExcelFill {
    /// Pattern type (`"none"`, `"gray125"`, `"solid"`, etc.).
    pub pattern_type: String,
    /// Foreground fill color (`fgColor`).
    pub fg_color: Option<Rgba>,
    /// Background color (`bgColor`).
    pub bg_color: Option<Rgba>,
}

/// Single border edge in [`ExcelBorder`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BorderEdge {
    /// Border style (`"thin"`, `"medium"`, `"thick"`, `"dashed"`, `"dotted"`, `"double"`).
    pub style: String,
    /// Border color.
    pub color: Rgba,
    /// Width in points.
    pub width_pt: f32,
}

/// Four-sided cell border definition in [`WorkbookStyles::borders`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExcelBorder {
    /// Left border.
    pub left: Option<BorderEdge>,
    /// Right border.
    pub right: Option<BorderEdge>,
    /// Top border.
    pub top: Option<BorderEdge>,
    /// Bottom border.
    pub bottom: Option<BorderEdge>,
}

/// Horizontal cell alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CellHAlign {
    /// Value-type dependent (`left` for text, `right` for numbers, `center` for bool/error).
    #[default]
    General,
    /// Left-aligned.
    Left,
    /// Centered.
    Center,
    /// Right-aligned.
    Right,
    /// Fill / justified.
    Justify,
}

/// Vertical cell alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CellVAlign {
    /// Top-aligned.
    Top,
    /// Vertically centered.
    Center,
    /// Bottom-aligned (Excel default).
    #[default]
    Bottom,
}

/// Resolved cell format record (`<xf>` in `cellXfs` or BIFF8 `XF`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CellXf {
    /// Number format ID (`numFmtId`).
    pub num_fmt_id: u16,
    /// Font index in [`WorkbookStyles::fonts`].
    pub font_id: u32,
    /// Fill index in [`WorkbookStyles::fills`].
    pub fill_id: u32,
    /// Border index in [`WorkbookStyles::borders`].
    pub border_id: u32,
    /// Horizontal alignment.
    pub h_align: CellHAlign,
    /// Vertical alignment.
    pub v_align: CellVAlign,
    /// Wrap text (`wrapText="1"`).
    pub wrap_text: bool,
    /// Shrink to fit (`shrinkToFit="1"`).
    pub shrink_to_fit: bool,
    /// Indent level (`indent`).
    pub indent: u8,
    /// Text rotation angle (`0..=180` or `255` vertical).
    pub text_rotation: u16,
}

/// Shared workbook style tables (`xl/styles.xml` or BIFF8 `FORMAT`/`FONT`/`XF` records).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkbookStyles {
    /// Custom number format codes keyed by `numFmtId`.
    pub num_fmts: BTreeMap<u16, String>,
    /// Font table.
    pub fonts: Vec<ExcelFont>,
    /// Fill table.
    pub fills: Vec<ExcelFill>,
    /// Border table.
    pub borders: Vec<ExcelBorder>,
    /// Cell format records (`cellXfs`).
    pub cell_xfs: Vec<CellXf>,
    /// Differential styles (`dxfs`) for conditional formatting.
    pub dxfs: Vec<DxfStyle>,
}

impl WorkbookStyles {
    /// Look up the [`CellXf`] for `style_idx`.
    #[must_use]
    pub fn xf(&self, style_idx: u32) -> Option<&CellXf> {
        self.cell_xfs.get(style_idx as usize)
    }

    /// Look up the [`ExcelFont`] for `style_idx`.
    #[must_use]
    pub fn font_for_style(&self, style_idx: u32) -> Option<&ExcelFont> {
        let xf = self.xf(style_idx)?;
        self.fonts.get(xf.font_id as usize)
    }

    /// Look up the solid background fill [`Rgba`] for `style_idx`, if any.
    #[must_use]
    pub fn fill_color_for_style(&self, style_idx: u32) -> Option<Rgba> {
        let xf = self.xf(style_idx)?;
        let fill = self.fills.get(xf.fill_id as usize)?;
        if fill.pattern_type.eq_ignore_ascii_case("solid") {
            fill.fg_color.or(fill.bg_color)
        } else {
            None
        }
    }

    /// Look up the [`ExcelBorder`] for `style_idx`.
    #[must_use]
    pub fn border_for_style(&self, style_idx: u32) -> Option<&ExcelBorder> {
        let xf = self.xf(style_idx)?;
        self.borders.get(xf.border_id as usize)
    }

    /// Look up the number format string for `style_idx`, if any.
    #[must_use]
    pub fn num_fmt_for_style(&self, style_idx: u32) -> Option<String> {
        let xf = self.xf(style_idx)?;
        if let Some(custom) = self.num_fmts.get(&xf.num_fmt_id) {
            return Some(custom.clone());
        }
        prism_numfmt::builtin_format_code(u32::from(xf.num_fmt_id)).map(str::to_string)
    }
}

/// Defined name / named range (`<definedName>` or BIFF8 `LBL`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefinedName {
    /// Name (e.g. `"_xlnm.Print_Area"`, `"TaxRate"`).
    pub name: String,
    /// Formula or range reference text (e.g. `"Sheet1!$A$1:$G$40"`).
    pub formula: String,
    /// Optional 0-based local sheet index (`localSheetId`).
    pub local_sheet_id: Option<u32>,
    /// Whether this defined name is hidden (`hidden="1"`).
    pub hidden: bool,
}

/// A single worksheet inside a [`Workbook`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sheet {
    /// Tab name.
    pub name: String,
    /// Sheet ID (`sheetId`).
    pub sheet_id: u32,
    /// Visibility (`Visible`, `Hidden`, `VeryHidden`).
    pub visibility: SheetVisibility,
    /// Default column width in points (default `48.0 pt` ≈ 8.43 chars).
    pub default_col_width_pt: f32,
    /// Default row height in points (default `15.0 pt`).
    pub default_row_height_pt: f32,
    /// Compressed column spans.
    pub col_spans: Vec<ColumnSpan>,
    /// Explicit row properties keyed by 0-based row index.
    pub row_props: BTreeMap<u32, RowProps>,
    /// Sparse cell map keyed by `(row_0based, col_0based)`.
    pub cells: BTreeMap<(u32, u32), Cell>,
    /// Merged cell ranges.
    pub merged_ranges: Vec<CellRange>,
    /// Frozen/split pane configuration.
    pub pane: Option<PaneInfo>,
    /// Comments keyed by `(row_0based, col_0based)`.
    pub comments: BTreeMap<(u32, u32), SheetComment>,
    /// Structured tables (`xl/tables/table*.xml`).
    pub tables: Vec<SheetTable>,
    /// Sheet-level autofilter range.
    pub auto_filter: Option<CellRange>,
    /// Conditional formatting rules.
    pub conditional_rules: Vec<ConditionalFormatRule>,
    /// Anchored drawings (charts, images, shapes).
    pub drawings: Vec<AnchoredDrawing>,
    /// Print setup.
    pub print_setup: PrintSetup,
    /// Whether the sheet is protected (`<sheetProtection>`).
    pub protected: bool,
    /// Optional tab color.
    pub tab_color: Option<Rgba>,
    /// Whether gridlines are shown in view mode (`showGridLines`).
    pub show_gridlines: bool,
    /// Whether row/column headers are shown in view mode (`showRowColHeaders`).
    pub show_row_col_headers: bool,
}

impl Default for Sheet {
    fn default() -> Self {
        Self {
            name: "Sheet1".to_string(),
            sheet_id: 1,
            visibility: SheetVisibility::Visible,
            default_col_width_pt: 48.0,
            default_row_height_pt: 15.0,
            col_spans: Vec::new(),
            row_props: BTreeMap::new(),
            cells: BTreeMap::new(),
            merged_ranges: Vec::new(),
            pane: None,
            comments: BTreeMap::new(),
            tables: Vec::new(),
            auto_filter: None,
            conditional_rules: Vec::new(),
            drawings: Vec::new(),
            print_setup: PrintSetup::default(),
            protected: false,
            tab_color: None,
            show_gridlines: true,
            show_row_col_headers: true,
        }
    }
}

impl Sheet {
    /// Return the width in points and hidden flag for `col` (0-based).
    #[must_use]
    pub fn col_width_and_hidden(&self, col: u32) -> (f32, bool) {
        for span in &self.col_spans {
            if col >= span.min_col && col <= span.max_col {
                return (span.width_pt, span.hidden);
            }
        }
        (self.default_col_width_pt, false)
    }

    /// Return the height in points and hidden flag for `row` (0-based).
    #[must_use]
    pub fn row_height_and_hidden(&self, row: u32) -> (f32, bool) {
        if let Some(rp) = self.row_props.get(&row) {
            (rp.height_pt, rp.hidden)
        } else {
            (self.default_row_height_pt, false)
        }
    }

    /// Compute the bounding `(max_row, max_col)` (0-based, inclusive) of populated content
    /// (cells, merged ranges, and drawings), or `None` if the sheet is completely empty.
    #[must_use]
    pub fn used_bounds(&self) -> Option<(u32, u32)> {
        let mut max_r: Option<u32> = None;
        let mut max_c: Option<u32> = None;
        for &(r, c) in self.cells.keys() {
            max_r = Some(max_r.map_or(r, |m| m.max(r)));
            max_c = Some(max_c.map_or(c, |m| m.max(c)));
        }
        for mr in &self.merged_ranges {
            max_r = Some(max_r.map_or(mr.end_row, |m| m.max(mr.end_row)));
            max_c = Some(max_c.map_or(mr.end_col, |m| m.max(mr.end_col)));
        }
        for d in &self.drawings {
            let er = d
                .anchor
                .to_row
                .unwrap_or(d.anchor.from_row.saturating_add(10));
            let ec = d
                .anchor
                .to_col
                .unwrap_or(d.anchor.from_col.saturating_add(5));
            max_r = Some(max_r.map_or(er, |m| m.max(er)));
            max_c = Some(max_c.map_or(ec, |m| m.max(ec)));
        }
        match (max_r, max_c) {
            (Some(r), Some(c)) => Some((r, c)),
            _ => None,
        }
    }
}

/// Forensic facts extracted from an Excel workbook (`prism_document_forensic_facts`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkbookForensicFacts {
    /// Names of visible worksheets.
    pub visible_sheet_names: Vec<String>,
    /// Names of user-hidden worksheets (`state="hidden"`).
    pub hidden_sheet_names: Vec<String>,
    /// Names of VBA-hidden worksheets (`state="veryHidden"`).
    pub very_hidden_sheet_names: Vec<String>,
    /// Total count of hidden rows across all worksheets.
    pub hidden_row_count: usize,
    /// Total count of hidden columns across all worksheets.
    pub hidden_col_count: usize,
    /// Count of cells concealed by `;;;` number formatting or matching font/fill colors.
    pub concealed_cell_count: usize,
    /// Count of formula cells that had no cached `<v>` value in the file.
    pub formulas_without_cached_value: usize,
    /// Count of `type="expression"` conditional formatting rules recorded without evaluation.
    pub unevaluated_conditional_rules: usize,
    /// External workbook links, web queries, and DDE/OLE references (never fetched).
    pub external_links: Vec<String>,
    /// Count of defined names (`<definedName>` / `LBL`).
    pub defined_names_count: usize,
    /// Whether `xl/vbaProject.bin` or a `_VBA_PROJECT_CUR` storage is present.
    pub vba_project_present: bool,
    /// Distinct comment authors.
    pub comment_authors: Vec<String>,
    /// Workbook calculation engine ID (`<calcPr calcId="..."/>`).
    pub calc_id: Option<String>,
    /// Whether workbook structure protection is active (`<workbookProtection>`).
    pub workbook_protected: bool,
    /// Names of worksheets with `<sheetProtection>` active.
    pub protected_sheet_names: Vec<String>,
}

/// Complete sparse `Excel` workbook model (`Workbook`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Workbook {
    /// Optional document title (`dc:title`).
    pub title: Option<String>,
    /// Optional document author (`dc:creator`).
    pub author: Option<String>,
    /// Date serial epoch (`Date1900` default or `Date1904`).
    pub date_system: DateSystem,
    /// Worksheets in workbook tab order (including `Hidden` and `VeryHidden`).
    pub sheets: Vec<Sheet>,
    /// Defined names (`_xlnm.Print_Area`, `_xlnm.Print_Titles`, named ranges).
    pub defined_names: Vec<DefinedName>,
    /// Shared style tables.
    pub styles: WorkbookStyles,
    /// Workbook theme (`xl/theme/theme1.xml`).
    pub theme: DrawingTheme,
    /// Embedded images keyed by resource ID.
    pub images: BTreeMap<String, ImageResource>,
    /// Forensic facts summary.
    pub forensic: WorkbookForensicFacts,
}

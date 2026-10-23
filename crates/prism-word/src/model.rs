// SPDX-License-Identifier: AGPL-3.0-only
//! Rich Word document model (`WordDocument`) representing `WordprocessingML` (`.docx`)
//! and legacy binary Word (`.doc`) semantics.

use prism_core::document::{FontResource, ImageResource};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Tri-state toggle property (`w:b`, `w:i`, `w:caps`, `w:smallCaps`, `w:strike`, `w:dstrike`, `w:vanish`).
///
/// In `WordprocessingML` (ECMA-376 §17.7.3):
/// - Within style levels, [`ToggleProp::Toggle`] XORs the inherited boolean value.
/// - In direct formatting (`w:r/w:rPr`), [`ToggleProp::Toggle`] sets the property to `true`.
/// - [`ToggleProp::Off`] (`w:val="0"` / `"false"` / `"off"`) explicitly sets the property to `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToggleProp {
    /// Property is toggled on (`<w:b/>`, `<w:b w:val="1"/>`, `<w:b w:val="true"/>`).
    Toggle,
    /// Property is explicitly turned off (`<w:b w:val="0"/>`, `<w:b w:val="false"/>`).
    Off,
}

impl ToggleProp {
    /// Apply this toggle within a style inheritance chain (XOR when `Toggle`, `false` when `Off`).
    #[must_use]
    pub const fn apply_in_style(self, current: bool) -> bool {
        match self {
            Self::Toggle => !current,
            Self::Off => false,
        }
    }

    /// Apply this toggle in direct formatting (`true` when `Toggle`, `false` when `Off`).
    #[must_use]
    pub const fn apply_direct(self) -> bool {
        match self {
            Self::Toggle => true,
            Self::Off => false,
        }
    }
}

/// Underline style (`w:u`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnderlineKind {
    /// No underline (`w:val="none"`).
    None,
    /// Single solid underline (`w:val="single"`).
    Single,
    /// Double underline (`w:val="double"`).
    Double,
    /// Dotted underline (`w:val="dotted"`, also used for hidden text in review mode).
    Dotted,
    /// Dashed underline (`w:val="dash"`).
    Dashed,
    /// Wavy underline (`w:val="wave"`).
    Wave,
    /// Words-only underline (`w:val="words"`).
    Words,
}

/// Vertical run alignment (`w:vertAlign`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerticalRunAlign {
    /// Normal baseline.
    Baseline,
    /// Superscript (`w:val="superscript"`).
    Superscript,
    /// Subscript (`w:val="subscript"`).
    Subscript,
}

/// Font slot mapping (`w:rFonts`) across character classes and theme references.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FontSlots {
    /// ASCII (U+0000..U+007F) font family.
    pub ascii: Option<String>,
    /// High-ANSI font family.
    pub h_ansi: Option<String>,
    /// East Asian font family.
    pub east_asia: Option<String>,
    /// Complex-script (`BiDi`) font family.
    pub cs: Option<String>,
    /// Theme font reference for ASCII (`majorHAnsi`, `minorHAnsi`, etc.).
    pub ascii_theme: Option<String>,
    /// Theme font reference for High-ANSI.
    pub h_ansi_theme: Option<String>,
    /// Theme font reference for East Asian.
    pub east_asia_theme: Option<String>,
    /// Theme font reference for Complex Script.
    pub cs_theme: Option<String>,
    /// Font slot hint (`default`, `eastAsia`, `cs`).
    pub hint: Option<String>,
}

/// Unresolved / delta run properties (`w:rPr`) at a single level (defaults, style, or direct).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunProperties {
    /// Character style reference (`w:rStyle`).
    pub style_id: Option<String>,
    /// Font family slots (`w:rFonts`).
    pub fonts: FontSlots,
    /// Bold (`w:b`).
    pub bold: Option<ToggleProp>,
    /// Complex-script bold (`w:bCs`).
    pub bold_cs: Option<ToggleProp>,
    /// Italic (`w:i`).
    pub italic: Option<ToggleProp>,
    /// Complex-script italic (`w:iCs`).
    pub italic_cs: Option<ToggleProp>,
    /// All-caps (`w:caps`).
    pub caps: Option<ToggleProp>,
    /// Small-caps (`w:smallCaps`).
    pub small_caps: Option<ToggleProp>,
    /// Single strikethrough (`w:strike`).
    pub strike: Option<ToggleProp>,
    /// Double strikethrough (`w:dstrike`).
    pub double_strike: Option<ToggleProp>,
    /// Hidden text (`w:vanish` / `w:specVanish`).
    pub vanish: Option<ToggleProp>,
    /// Underline style (`w:u`).
    pub underline: Option<UnderlineKind>,
    /// Underline color (`#RRGGBB`).
    pub underline_color: Option<String>,
    /// Font size in points (`w:sz`, converted from half-points).
    pub font_size_pt: Option<f32>,
    /// Complex-script font size in points (`w:szCs`).
    pub font_size_cs_pt: Option<f32>,
    /// Resolved foreground text color (`#RRGGBB`).
    pub color: Option<String>,
    /// Highlight / shading background color (`#RRGGBB`).
    pub highlight_color: Option<String>,
    /// Character spacing adjustment in points (`w:spacing`, from twips).
    pub char_spacing_pt: Option<f32>,
    /// Baseline position offset in points (`w:position`, from half-points).
    pub position_pt: Option<f32>,
    /// Superscript / subscript (`w:vertAlign`).
    pub vert_align: Option<VerticalRunAlign>,
}

/// Fully resolved run style after evaluating the inheritance hierarchy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ResolvedRunStyle {
    /// Primary (ASCII / Latin) font family.
    pub font_family: String,
    /// East Asian font family fallback.
    pub east_asia_font: Option<String>,
    /// Complex-script font family fallback.
    pub cs_font: Option<String>,
    /// Font size in points.
    pub font_size_pt: f32,
    /// Bold state.
    pub bold: bool,
    /// Italic state.
    pub italic: bool,
    /// All-caps state.
    pub caps: bool,
    /// Small-caps state.
    pub small_caps: bool,
    /// Strikethrough state (`strike` or `double_strike`).
    pub strike: bool,
    /// Double-strikethrough state.
    pub double_strike: bool,
    /// Hidden (`w:vanish`) state.
    pub vanish: bool,
    /// Underline kind.
    pub underline: UnderlineKind,
    /// Foreground color (`#RRGGBB`).
    pub color: String,
    /// Highlight / background color (`#RRGGBB`).
    pub highlight_color: Option<String>,
    /// Character spacing adjustment in points.
    pub char_spacing_pt: f32,
    /// Vertical baseline alignment.
    pub vert_align: VerticalRunAlign,
}

impl Default for ResolvedRunStyle {
    fn default() -> Self {
        Self {
            font_family: "Calibri".to_string(),
            east_asia_font: None,
            cs_font: None,
            font_size_pt: 11.0,
            bold: false,
            italic: false,
            caps: false,
            small_caps: false,
            strike: false,
            double_strike: false,
            vanish: false,
            underline: UnderlineKind::None,
            color: "#000000".to_string(),
            highlight_color: None,
            char_spacing_pt: 0.0,
            vert_align: VerticalRunAlign::Baseline,
        }
    }
}

/// Paragraph horizontal alignment (`w:jc`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WordAlignment {
    /// Left / start aligned.
    Left,
    /// Centered.
    Center,
    /// Right / end aligned.
    Right,
    /// Justified (`both`).
    Justify,
    /// Distributed (`distribute`).
    Distribute,
}

/// Paragraph line spacing rule (`w:spacing/@w:lineRule`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum WordLineSpacing {
    /// Proportional line spacing (`240` = `1.0` single spacing).
    Auto(f32),
    /// Exact line height in points (`w:lineRule="exact"`).
    ExactPt(f32),
    /// Minimum line height in points (`w:lineRule="atLeast"`).
    AtLeastPt(f32),
}

/// Custom tab stop alignment (`w:tab/@w:val`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TabStopKind {
    /// Left-aligned tab stop.
    Left,
    /// Center-aligned tab stop.
    Center,
    /// Right-aligned tab stop.
    Right,
    /// Decimal-aligned tab stop.
    Decimal,
    /// Vertical bar tab stop.
    Bar,
    /// Clear an inherited tab stop at this position.
    Clear,
}

/// Tab leader character (`w:tab/@w:leader`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TabLeader {
    /// No leader (`none`).
    None,
    /// Dotted leader (`dot`).
    Dot,
    /// Hyphen/dashed leader (`hyphen`).
    Hyphen,
    /// Underscore solid leader (`underscore`).
    Underscore,
}

/// A custom tab stop (`w:tab`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TabStop {
    /// Position in points from the left margin/column edge.
    pub position_pt: f32,
    /// Alignment kind.
    pub kind: TabStopKind,
    /// Leader fill style.
    pub leader: TabLeader,
}

/// Border stroke style (`w:val` on border elements).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum WordBorderStyle {
    /// Explicitly no border (`none` or `nil`).
    None,
    /// Dotted border.
    Dotted,
    /// Dashed border.
    Dashed,
    /// Single solid line.
    Single,
    /// Thick solid line.
    Thick,
    /// Double line.
    Double,
}

/// A single border specification (`w:top`, `w:bottom`, `w:left`, `w:right`, `w:between`, `w:insideH`, `w:insideV`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordBorder {
    /// Border line style.
    pub style: WordBorderStyle,
    /// Width in points (`w:sz` is in eighths of a point).
    pub width_pt: f32,
    /// Color (`#RRGGBB`).
    pub color: String,
    /// Padding space in points (`w:space`).
    pub space_pt: f32,
}

/// Paragraph or cell border set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WordBorders {
    /// Top border.
    pub top: Option<WordBorder>,
    /// Right border.
    pub right: Option<WordBorder>,
    /// Bottom border.
    pub bottom: Option<WordBorder>,
    /// Left border.
    pub left: Option<WordBorder>,
    /// Border between adjacent paragraphs with identical border definitions.
    pub between: Option<WordBorder>,
    /// Inside horizontal border (tables).
    pub inside_h: Option<WordBorder>,
    /// Inside vertical border (tables).
    pub inside_v: Option<WordBorder>,
}

/// Numbering reference attached to a paragraph (`w:numPr`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NumberingRef {
    /// Numbering instance ID (`w:numId`).
    pub num_id: u32,
    /// 0-based indentation level (`w:ilvl`, 0..=8).
    pub ilvl: u8,
}

/// Unresolved / delta paragraph properties (`w:pPr`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ParagraphProperties {
    /// Paragraph style ID (`w:pStyle`).
    pub style_id: Option<String>,
    /// Horizontal alignment (`w:jc`).
    pub alignment: Option<WordAlignment>,
    /// Space before paragraph in points (`w:spacing/@w:before`).
    pub space_before_pt: Option<f32>,
    /// Space after paragraph in points (`w:spacing/@w:after`).
    pub space_after_pt: Option<f32>,
    /// Automatic before-spacing (`w:beforeAutospacing`).
    pub space_before_auto: Option<bool>,
    /// Automatic after-spacing (`w:afterAutospacing`).
    pub space_after_auto: Option<bool>,
    /// Line spacing rule (`w:spacing/@w:line` and `@w:lineRule`).
    pub line_spacing: Option<WordLineSpacing>,
    /// Suppress spacing between paragraphs of the same style (`w:contextualSpacing`).
    pub contextual_spacing: Option<bool>,
    /// Left indent in points (`w:ind/@w:left` or `@w:start`).
    pub indent_left_pt: Option<f32>,
    /// Right indent in points (`w:ind/@w:right` or `@w:end`).
    pub indent_right_pt: Option<f32>,
    /// First-line indent in points (`+` for firstLine, `-` for hanging).
    pub indent_first_line_pt: Option<f32>,
    /// Keep with next paragraph (`w:keepNext`).
    pub keep_next: Option<bool>,
    /// Keep lines of this paragraph together on one page (`w:keepLines`).
    pub keep_lines: Option<bool>,
    /// Force page break before paragraph (`w:pageBreakBefore`).
    pub page_break_before: Option<bool>,
    /// Widow/orphan control (`w:widowControl`; defaults to `true` in Word).
    pub widow_control: Option<bool>,
    /// Suppress automatic hyphenation (`w:suppressAutoHyphens`).
    pub suppress_auto_hyphens: Option<bool>,
    /// Outline level (`0..=8` for Headings 1..9, `9` for body text).
    pub outline_level: Option<u8>,
    /// Numbering reference (`w:numPr`).
    pub numbering: Option<NumberingRef>,
    /// Custom tab stops (`w:tabs/w:tab`).
    pub tab_stops: Vec<TabStop>,
    /// Paragraph borders (`w:pBdr`).
    pub borders: WordBorders,
    /// Paragraph background shading fill (`w:shd/@w:fill`, `#RRGGBB`).
    pub shading_fill: Option<String>,
    /// Paragraph mark run properties (`w:pPr/w:rPr`).
    pub mark_run_props: Option<RunProperties>,
    /// Mid-document section break properties (`w:pPr/w:sectPr`).
    pub section_break: Option<Box<SectionProperties>>,
}

/// Fully resolved paragraph formatting after inheritance evaluation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ResolvedParagraphStyle {
    /// Effective style ID, if any.
    pub style_id: Option<String>,
    /// Horizontal alignment.
    pub alignment: WordAlignment,
    /// Space before in points.
    pub space_before_pt: f32,
    /// Space after in points.
    pub space_after_pt: f32,
    /// Line spacing rule.
    pub line_spacing: WordLineSpacing,
    /// Suppress spacing between paragraphs of the same style.
    pub contextual_spacing: bool,
    /// Left indent in points.
    pub indent_left_pt: f32,
    /// Right indent in points.
    pub indent_right_pt: f32,
    /// First-line indent in points (negative = hanging indent).
    pub indent_first_line_pt: f32,
    /// Keep with next paragraph.
    pub keep_next: bool,
    /// Keep lines together.
    pub keep_lines: bool,
    /// Page break before paragraph.
    pub page_break_before: bool,
    /// Widow/orphan control enabled.
    pub widow_control: bool,
    /// Outline level (`0..=8` for heading levels).
    pub outline_level: Option<u8>,
    /// Resolved numbering reference.
    pub numbering: Option<NumberingRef>,
    /// Resolved custom tab stops sorted by position.
    pub tab_stops: Vec<TabStop>,
    /// Resolved paragraph borders.
    pub borders: WordBorders,
    /// Resolved shading fill (`#RRGGBB`).
    pub shading_fill: Option<String>,
}

impl Default for ResolvedParagraphStyle {
    fn default() -> Self {
        Self {
            style_id: None,
            alignment: WordAlignment::Left,
            space_before_pt: 0.0,
            space_after_pt: 8.0,
            line_spacing: WordLineSpacing::Auto(1.08),
            contextual_spacing: false,
            indent_left_pt: 0.0,
            indent_right_pt: 0.0,
            indent_first_line_pt: 0.0,
            keep_next: false,
            keep_lines: false,
            page_break_before: false,
            widow_control: true,
            outline_level: None,
            numbering: None,
            tab_stops: Vec::new(),
            borders: WordBorders::default(),
            shading_fill: None,
        }
    }
}

/// Word style category (`w:style/@w:type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StyleType {
    /// Paragraph style (`paragraph`).
    Paragraph,
    /// Character style (`character`).
    Character,
    /// Table style (`table`).
    Table,
    /// Numbering style (`numbering`).
    Numbering,
}

/// Conditional table style region (`w:tblStylePr/@w:type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TableStyleRegion {
    /// Entire table (`wholeTable`).
    WholeTable,
    /// Odd vertical column band (`band1Vert`).
    Band1Vert,
    /// Even vertical column band (`band2Vert`).
    Band2Vert,
    /// Odd horizontal row band (`band1Horz`).
    Band1Horz,
    /// Even horizontal row band (`band2Horz`).
    Band2Horz,
    /// Header row (`firstRow`).
    FirstRow,
    /// Total / footer row (`lastRow`).
    LastRow,
    /// First column (`firstCol`).
    FirstCol,
    /// Last column (`lastCol`).
    LastCol,
    /// Top-left cell (`nwCell`).
    NwCell,
    /// Top-right cell (`neCell`).
    NeCell,
    /// Bottom-left cell (`swCell`).
    SwCell,
    /// Bottom-right cell (`seCell`).
    SeCell,
}

/// Conditional formatting override for a specific [`TableStyleRegion`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TableStyleConditional {
    /// Paragraph property overrides for cells in this region.
    pub para_props: ParagraphProperties,
    /// Run property overrides for cells in this region.
    pub run_props: RunProperties,
    /// Cell border overrides.
    pub borders: WordBorders,
    /// Cell shading fill override (`#RRGGBB`).
    pub shading_fill: Option<String>,
}

/// A style definition in `word/styles.xml` or `STSH`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordStyle {
    /// Unique style identifier (`w:styleId`).
    pub id: String,
    /// Primary display name (`w:name`).
    pub name: Option<String>,
    /// Style category.
    pub style_type: StyleType,
    /// Whether this is the default style for its [`StyleType`] (`w:default="1"`).
    pub is_default: bool,
    /// Parent style ID (`w:basedOn`).
    pub based_on: Option<String>,
    /// Next-paragraph style ID (`w:next`).
    pub next: Option<String>,
    /// Linked character/paragraph style ID (`w:link`).
    pub link: Option<String>,
    /// Paragraph properties defined on this style.
    pub para_props: ParagraphProperties,
    /// Run properties defined on this style.
    pub run_props: RunProperties,
    /// Table borders (for table styles).
    pub table_borders: WordBorders,
    /// Conditional region overrides (for table styles).
    pub table_conditionals: BTreeMap<TableStyleRegion, TableStyleConditional>,
}

/// Document stylesheet (`word/styles.xml` or `STSH`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WordStyleSheet {
    /// Document-wide default paragraph properties (`w:docDefaults/w:pPrDefault`).
    pub default_para_props: ParagraphProperties,
    /// Document-wide default run properties (`w:docDefaults/w:rPrDefault`).
    pub default_run_props: RunProperties,
    /// Styles keyed by `styleId`.
    pub styles: BTreeMap<String, WordStyle>,
}

/// Numbering format (`w:numFmt`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NumberFormat {
    /// `1, 2, 3` (`decimal`).
    Decimal,
    /// `01, 02, 03` (`decimalZero`).
    DecimalZero,
    /// `I, II, III` (`upperRoman`).
    UpperRoman,
    /// `i, ii, iii` (`lowerRoman`).
    LowerRoman,
    /// `A, B, C` (`upperLetter`).
    UpperLetter,
    /// `a, b, c` (`lowerLetter`).
    LowerLetter,
    /// `1st, 2nd, 3rd` (`ordinal`).
    Ordinal,
    /// Bullet character (`bullet`).
    Bullet,
    /// No number/bullet (`none`).
    None,
    /// Other named format.
    Custom(String),
}

/// Suffix placed after a list marker (`w:suff`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LevelSuffix {
    /// Tab character (`tab`, default).
    Tab,
    /// Single space (`space`).
    Space,
    /// Nothing (`nothing`).
    Nothing,
}

/// Definition of a single numbering level (`w:lvl`, `0..=8`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LevelDef {
    /// 0-based level index (`0..=8`).
    pub ilvl: u8,
    /// Starting value (`w:start`, defaults to `1`).
    pub start: i32,
    /// Number format (`w:numFmt`).
    pub num_fmt: NumberFormat,
    /// Level text template (`w:lvlText`, e.g. `"%1."`, `"%1.%2."`, or bullet glyph).
    pub lvl_text: String,
    /// Level justification (`w:lvlJc`).
    pub alignment: WordAlignment,
    /// Suffix after marker (`w:suff`).
    pub suffix: LevelSuffix,
    /// Use legal (all-Arabic) numbering across levels (`w:isLgl`).
    pub is_legal: bool,
    /// Restart level after higher level (`w:lvlRestart`).
    pub restart_after_level: Option<u8>,
    /// Associated paragraph style ID (`w:pStyle`).
    pub para_style_id: Option<String>,
    /// Level paragraph properties (`w:pPr`).
    pub para_props: ParagraphProperties,
    /// Level run properties for the marker (`w:rPr`).
    pub run_props: RunProperties,
}

/// Abstract numbering definition (`w:abstractNum`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AbstractNum {
    /// Abstract numbering ID (`w:abstractNumId`).
    pub id: u32,
    /// Style link to a numbering style (`w:numStyleLink`).
    pub num_style_link: Option<String>,
    /// Levels keyed by `ilvl` (`0..=8`).
    pub levels: BTreeMap<u8, LevelDef>,
}

/// Per-level override on a concrete `w:num` (`w:lvlOverride`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LevelOverride {
    /// 0-based level index.
    pub ilvl: u8,
    /// Optional start value override (`w:startOverride`).
    pub start_override: Option<i32>,
    /// Optional replacement level definition (`w:lvl`).
    pub level_def: Option<LevelDef>,
}

/// Concrete numbering instance (`w:num`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NumInstance {
    /// Concrete numbering ID (`w:numId`).
    pub num_id: u32,
    /// Referenced `w:abstractNumId`.
    pub abstract_num_id: u32,
    /// Level overrides keyed by `ilvl`.
    pub overrides: BTreeMap<u8, LevelOverride>,
}

/// Document numbering table (`word/numbering.xml` or `PlfLst` / `PlfLfo`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WordNumbering {
    /// Abstract numbering definitions keyed by `abstractNumId`.
    pub abstract_nums: BTreeMap<u32, AbstractNum>,
    /// Concrete numbering instances keyed by `numId`.
    pub nums: BTreeMap<u32, NumInstance>,
}

/// Office theme (`word/theme/theme1.xml`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WordTheme {
    /// Theme name.
    pub name: String,
    /// Color scheme slots (`dk1`, `lt1`, `dk2`, `lt2`, `accent1`..`accent6`, `hlink`, `folHlink`) -> 6-digit hex (`RRGGBB`).
    pub colors: BTreeMap<String, String>,
    /// Major (heading) Latin typeface.
    pub major_latin: Option<String>,
    /// Major East Asian typeface.
    pub major_east_asia: Option<String>,
    /// Major Complex Script typeface.
    pub major_cs: Option<String>,
    /// Minor (body) Latin typeface.
    pub minor_latin: Option<String>,
    /// Minor East Asian typeface.
    pub minor_east_asia: Option<String>,
    /// Minor Complex Script typeface.
    pub minor_cs: Option<String>,
}

/// Document-level settings (`word/settings.xml` or `DOP`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct WordSettings {
    /// Word compatibility mode (`11` = 2003, `12` = 2007, `14` = 2010, `15` = 2013+).
    pub compatibility_mode: u16,
    /// Named compatibility flags from `<w:compat>`.
    pub compat_flags: BTreeMap<String, bool>,
    /// Default tab stop interval in points (`w:defaultTabStop`, defaults to `36.0` pt = 0.5 in).
    pub default_tab_stop_pt: f32,
    /// Different odd and even pages headers/footers (`w:evenAndOddHeaders`).
    pub even_and_odd_headers: bool,
    /// Mirror left/right margins on facing pages (`w:mirrorMargins`).
    pub mirror_margins: bool,
    /// Automatic hyphenation (`w:autoHyphenation`).
    pub auto_hyphenation: bool,
    /// Track revisions enabled (`w:trackRevisions`).
    pub track_revisions: bool,
}

impl Default for WordSettings {
    fn default() -> Self {
        Self {
            compatibility_mode: 15,
            compat_flags: BTreeMap::new(),
            default_tab_stop_pt: 36.0,
            even_and_odd_headers: false,
            mirror_margins: false,
            auto_hyphenation: false,
            track_revisions: false,
        }
    }
}

/// Section break kind (`w:sectPr/w:type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SectionBreakKind {
    /// Start on the next page (`nextPage`, default).
    NextPage,
    /// Continuous break on the same page (`continuous`).
    Continuous,
    /// Start on the next even-numbered page (`evenPage`).
    EvenPage,
    /// Start on the next odd-numbered page (`oddPage`).
    OddPage,
}

/// A column definition in a multi-column section (`w:cols/w:col`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ColumnDef {
    /// Column width in points.
    pub width_pt: f32,
    /// Space after this column in points.
    pub space_pt: f32,
}

/// Header or footer slot type (`w:headerReference` / `w:footerReference` `@w:type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum HeaderFooterKind {
    /// Default (odd-page or all-page) header/footer (`default`).
    Default,
    /// First-page header/footer (`first`, used when `title_pg` is true).
    First,
    /// Even-page header/footer (`even`, used when `even_and_odd_headers` is true).
    Even,
}

/// Vertical page alignment (`w:vAlign`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageVerticalAlign {
    /// Top-aligned (default).
    Top,
    /// Centered vertically.
    Center,
    /// Justified vertically.
    Both,
    /// Bottom-aligned.
    Bottom,
}

/// Section properties (`w:sectPr` or `SEPX`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SectionProperties {
    /// Section break kind.
    pub break_kind: SectionBreakKind,
    /// Page width in points (`w:pgSz/@w:w`).
    pub page_width_pt: f32,
    /// Page height in points (`w:pgSz/@w:h`).
    pub page_height_pt: f32,
    /// Landscape orientation (`w:pgSz/@w:orient="landscape"`).
    pub landscape: bool,
    /// Top page margin in points (`w:pgMar/@w:top`).
    pub margin_top_pt: f32,
    /// Right page margin in points (`w:pgMar/@w:right`).
    pub margin_right_pt: f32,
    /// Bottom page margin in points (`w:pgMar/@w:bottom`).
    pub margin_bottom_pt: f32,
    /// Left page margin in points (`w:pgMar/@w:left`).
    pub margin_left_pt: f32,
    /// Header distance from top page edge in points (`w:pgMar/@w:header`).
    pub header_margin_pt: f32,
    /// Footer distance from bottom page edge in points (`w:pgMar/@w:footer`).
    pub footer_margin_pt: f32,
    /// Gutter margin in points (`w:pgMar/@w:gutter`).
    pub gutter_pt: f32,
    /// Number of equal columns (`w:cols/@w:num`, defaults to `1`).
    pub column_count: u16,
    /// Spacing between equal columns in points (`w:cols/@w:space`, defaults to `36.0` pt).
    pub column_space_pt: f32,
    /// Draw vertical separator line between columns (`w:cols/@w:sep`).
    pub column_separator: bool,
    /// Explicit unequal column definitions (`w:cols/w:col`).
    pub custom_columns: Vec<ColumnDef>,
    /// Different first page header/footer (`w:titlePg`).
    pub title_pg: bool,
    /// Vertical alignment of body content on the page (`w:vAlign`).
    pub vertical_align: PageVerticalAlign,
    /// Page number format (`w:pgNumType/@w:fmt`).
    pub page_num_format: NumberFormat,
    /// Optional starting page number (`w:pgNumType/@w:start`).
    pub page_num_start: Option<u32>,
    /// Header part IDs keyed by [`HeaderFooterKind`].
    pub headers: BTreeMap<HeaderFooterKind, String>,
    /// Footer part IDs keyed by [`HeaderFooterKind`].
    pub footers: BTreeMap<HeaderFooterKind, String>,
}

impl Default for SectionProperties {
    fn default() -> Self {
        Self {
            break_kind: SectionBreakKind::NextPage,
            page_width_pt: 612.0,
            page_height_pt: 792.0,
            landscape: false,
            margin_top_pt: 72.0,
            margin_right_pt: 72.0,
            margin_bottom_pt: 72.0,
            margin_left_pt: 72.0,
            header_margin_pt: 36.0,
            footer_margin_pt: 36.0,
            gutter_pt: 0.0,
            column_count: 1,
            column_space_pt: 36.0,
            column_separator: false,
            custom_columns: Vec::new(),
            title_pg: false,
            vertical_align: PageVerticalAlign::Top,
            page_num_format: NumberFormat::Decimal,
            page_num_start: None,
            headers: BTreeMap::new(),
            footers: BTreeMap::new(),
        }
    }
}

/// Revision type (`w:ins`, `w:del`, `w:moveFrom`, `w:moveTo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RevisionKind {
    /// Inserted content (`w:ins`).
    Inserted,
    /// Deleted content (`w:del`).
    Deleted,
    /// Moved source content (`w:moveFrom`).
    MoveFrom,
    /// Moved destination content (`w:moveTo`).
    MoveTo,
}

/// Metadata attached to a tracked revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WordRevisionMeta {
    /// Revision identifier (`w:id`).
    pub id: String,
    /// Revision kind.
    pub kind: RevisionKind,
    /// Author name (`w:author`).
    pub author: Option<String>,
    /// ISO-8601 date string (`w:date`).
    pub date: Option<String>,
}

/// Anchored drawing positioning mode (`wp:anchor`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnchorPlacement {
    /// Horizontal offset in points relative to `h_relative_from`.
    pub pos_x_pt: f32,
    /// Horizontal reference origin (`page`, `margin`, `column`, `character`).
    pub h_relative_from: String,
    /// Vertical offset in points relative to `v_relative_from`.
    pub pos_y_pt: f32,
    /// Vertical reference origin (`page`, `margin`, `paragraph`, `line`).
    pub v_relative_from: String,
    /// Behind document text (`behindDoc="1"`).
    pub behind_doc: bool,
    /// Allow overlapping other anchored objects (`allowOverlap="1"`).
    pub allow_overlap: bool,
    /// Layout inside table cell (`layoutInCell="1"`).
    pub layout_in_cell: bool,
    /// Relative Z-order (`relativeHeight`).
    pub z_order: i64,
}

/// Text wrapping mode around an anchored object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WrapMode {
    /// Inline with text (`wp:inline`).
    Inline,
    /// Floating with no text wrapping (`wp:wrapNone`).
    None,
    /// Rectangular wrap (`wp:wrapSquare`).
    Square,
    /// Tight polygon wrap (`wp:wrapTight`).
    Tight {
        /// Polygon vertices in normalized `[0, 21600]` coordinates.
        polygon: Vec<(f32, f32)>,
    },
    /// Through polygon wrap (`wp:wrapThrough`).
    Through {
        /// Polygon vertices in normalized `[0, 21600]` coordinates.
        polygon: Vec<(f32, f32)>,
    },
    /// Top-and-bottom wrap (`wp:wrapTopAndBottom`).
    TopAndBottom,
}

/// Payload inside a [`WordDrawing`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DrawingPayload {
    /// Raster or vector image referencing an [`ImageResource`] ID.
    Image {
        /// Relationship / resource ID.
        resource_id: String,
        /// Crop rectangle `(left, top, right, bottom)` as fractions `[0.0, 1.0]`.
        crop: Option<(f32, f32, f32, f32)>,
    },
    /// Text box (`wps:txbx` or `v:textbox`) containing nested Word blocks.
    TextBox {
        /// Blocks inside the text box.
        blocks: Vec<WordBlock>,
        /// Fill color (`#RRGGBB`).
        fill_color: Option<String>,
        /// Stroke color (`#RRGGBB`).
        stroke_color: Option<String>,
        /// Stroke width in points.
        stroke_width_pt: f32,
    },
    /// Labelled placeholder for a chart or OLE object without an image preview.
    Placeholder {
        /// Descriptive label.
        label: String,
    },
}

/// An inline or floating drawing object (`w:drawing`, `w:pict`, or VML).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordDrawing {
    /// Display width in points (`wp:extent/@cx`).
    pub width_pt: f32,
    /// Display height in points (`wp:extent/@cy`).
    pub height_pt: f32,
    /// Alternative text / description (`wp:docPr/@descr` or `@title`).
    pub alt_text: Option<String>,
    /// Rotation angle in degrees.
    pub rotation_deg: f32,
    /// Wrap mode.
    pub wrap: WrapMode,
    /// Optional floating anchor placement (`None` for inline drawings).
    pub anchor: Option<AnchorPlacement>,
    /// Drawing payload.
    pub payload: DrawingPayload,
}

/// Special non-text run item inside a [`WordRun`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RunItem {
    /// UTF-8 text span (`w:t` or `w:delText`).
    Text(String),
    /// Tab character (`w:tab`).
    Tab,
    /// Line break (`w:br` without page/column type, or `w:cr`).
    LineBreak,
    /// Explicit page break (`<w:br w:type="page"/>`).
    PageBreak,
    /// Explicit column break (`<w:br w:type="column"/>`).
    ColumnBreak,
    /// Soft (optional) hyphen (`w:softHyphen`, U+00AD).
    SoftHyphen,
    /// Non-breaking hyphen (`w:noBreakHyphen`, U+2011).
    NoBreakHyphen,
    /// Symbol glyph (`<w:sym w:font="..." w:char="..."/>`).
    Symbol {
        /// Symbol font family (`Symbol`, `Wingdings`, etc.).
        font: String,
        /// Hex character code.
        char_code: u16,
    },
    /// Footnote reference marker (`w:footnoteReference/@w:id`).
    FootnoteRef(i32),
    /// Endnote reference marker (`w:endnoteReference/@w:id`).
    EndnoteRef(i32),
    /// Inline or anchored drawing (`w:drawing` / `w:pict`).
    Drawing(WordDrawing),
}

/// A single character run (`w:r`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WordRun {
    /// Direct run properties (`w:rPr`).
    pub props: RunProperties,
    /// Ordered sequence of text and inline items in this run.
    pub items: Vec<RunItem>,
    /// Active revision metadata if this run is inside `w:ins`, `w:del`, `w:moveFrom`, or `w:moveTo`.
    pub revision: Option<WordRevisionMeta>,
}

/// Classification of a Word field instruction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldKind {
    /// Current page number (`PAGE`). Computed by layout.
    Page,
    /// Total pages in document (`NUMPAGES`). Computed by layout.
    NumPages,
    /// Total pages in current section (`SECTIONPAGES`). Computed by layout.
    SectionPages,
    /// Hyperlink (`HYPERLINK "..."`).
    Hyperlink(String),
    /// External image reference (`INCLUDEPICTURE "..."`). Never fetched; recorded forensically.
    IncludePicture(String),
    /// External text reference (`INCLUDETEXT "..."`). Never fetched; recorded forensically.
    IncludeText(String),
    /// Table of contents (`TOC`). Uses cached result only.
    Toc,
    /// Any other field (`DATE`, `TIME`, `AUTHOR`, `REF`, `MERGEFIELD`, etc.). Uses cached result only.
    Other(String),
}

/// A Word field (`w:fldSimple` or `w:fldChar` `begin`..`separate`..`end`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordField {
    /// Raw field instruction string (`w:instrText`).
    pub instruction: String,
    /// Parsed field classification.
    pub kind: FieldKind,
    /// Cached display runs between `separate` and `end`.
    pub cached_result: Vec<ParagraphContent>,
    /// Field is locked (`w:fldLock`).
    pub locked: bool,
    /// Field is marked dirty (`w:dirty`).
    pub dirty: bool,
}

/// A hyperlink span (`w:hyperlink`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordHyperlink {
    /// Target URL or internal anchor (data only; never navigated automatically).
    pub target: String,
    /// Tooltip string (`w:tooltip`).
    pub tooltip: Option<String>,
    /// Nested paragraph content inside the hyperlink.
    pub content: Vec<ParagraphContent>,
}

/// Inline content items inside a [`WordParagraph`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum ParagraphContent {
    /// A character run (`w:r`).
    Run(WordRun),
    /// A complex or simple field.
    Field(WordField),
    /// A hyperlink span (`w:hyperlink`).
    Hyperlink(WordHyperlink),
    /// Bookmark start anchor (`w:bookmarkStart`).
    BookmarkStart {
        /// Bookmark ID.
        id: String,
        /// Bookmark name.
        name: String,
    },
    /// Bookmark end marker (`w:bookmarkEnd`).
    BookmarkEnd {
        /// Bookmark ID.
        id: String,
    },
    /// Comment range start (`w:commentRangeStart`).
    CommentRangeStart(String),
    /// Comment range end (`w:commentRangeEnd`).
    CommentRangeEnd(String),
}

/// A paragraph (`w:p`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WordParagraph {
    /// Paragraph properties (`w:pPr`).
    pub props: ParagraphProperties,
    /// Inline content elements.
    pub content: Vec<ParagraphContent>,
}

impl WordParagraph {
    /// Collect all [`WordRun`]s in reading order (descending into cached field results and hyperlinks).
    #[must_use]
    pub fn runs(&self) -> Vec<&WordRun> {
        let mut out = Vec::new();
        Self::collect_runs(&self.content, &mut out);
        out
    }

    fn collect_runs<'a>(items: &'a [ParagraphContent], out: &mut Vec<&'a WordRun>) {
        for item in items {
            match item {
                ParagraphContent::Run(r) => out.push(r),
                ParagraphContent::Field(f) => Self::collect_runs(&f.cached_result, out),
                ParagraphContent::Hyperlink(h) => Self::collect_runs(&h.content, out),
                ParagraphContent::BookmarkStart { .. }
                | ParagraphContent::BookmarkEnd { .. }
                | ParagraphContent::CommentRangeStart(_)
                | ParagraphContent::CommentRangeEnd(_) => {}
            }
        }
    }
}

/// Width specification on a table or cell (`w:tblW`, `w:tcW`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TableWidth {
    /// Automatic width (`auto`).
    Auto,
    /// Fixed width in points (`dxa`, converted from twips).
    Pt(f32),
    /// Percentage of container width (`pct`, `0.0..=100.0`).
    Percent(f32),
}

/// Table layout algorithm (`w:tblLayout`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TableLayoutMode {
    /// Dynamic column sizing based on cell contents (`autofit`, default).
    AutoFit,
    /// Fixed column widths from `tblGrid` / `tcW` (`fixed`).
    Fixed,
}

/// Vertical merge state of a table cell (`w:vMerge`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerticalMerge {
    /// Not vertically merged.
    None,
    /// Starts a new vertical merge group (`w:val="restart"`).
    Restart,
    /// Continues a vertical merge group from the cell above (`<w:vMerge/>` or `w:val="continue"`).
    Continue,
}

/// Cell padding margins in points (`w:tblCellMar` or `w:tcMar`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CellMargins {
    /// Top padding in points.
    pub top_pt: f32,
    /// Right padding in points.
    pub right_pt: f32,
    /// Bottom padding in points.
    pub bottom_pt: f32,
    /// Left padding in points.
    pub left_pt: f32,
}

impl Default for CellMargins {
    fn default() -> Self {
        Self {
            top_pt: 0.0,
            right_pt: 5.4,
            bottom_pt: 0.0,
            left_pt: 5.4,
        }
    }
}

/// Table-level properties (`w:tblPr`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct TableProperties {
    /// Referenced table style ID (`w:tblStyle`).
    pub style_id: Option<String>,
    /// Preferred table width (`w:tblW`).
    pub preferred_width: TableWidth,
    /// Horizontal table alignment (`w:jc`).
    pub alignment: WordAlignment,
    /// Left table indent in points (`w:tblInd`).
    pub indent_left_pt: f32,
    /// Layout mode (`Fixed` vs `AutoFit`).
    pub layout_mode: TableLayoutMode,
    /// Default cell margins (`w:tblCellMar`).
    pub cell_margins: CellMargins,
    /// Table borders (`w:tblBorders`).
    pub borders: WordBorders,
    /// Table shading fill (`#RRGGBB`).
    pub shading_fill: Option<String>,
    /// Conditional formatting look flags (`w:tblLook`: `firstRow`, `lastRow`, `firstColumn`, `lastColumn`, `noHBand`, `noVBand`).
    pub look_first_row: bool,
    /// Apply `lastRow` conditional formatting.
    pub look_last_row: bool,
    /// Apply `firstCol` conditional formatting.
    pub look_first_col: bool,
    /// Apply `lastCol` conditional formatting.
    pub look_last_col: bool,
    /// Enable horizontal row banding.
    pub look_h_band: bool,
    /// Enable vertical column banding.
    pub look_v_band: bool,
}

impl Default for TableProperties {
    fn default() -> Self {
        Self {
            style_id: None,
            preferred_width: TableWidth::Auto,
            alignment: WordAlignment::Left,
            indent_left_pt: 0.0,
            layout_mode: TableLayoutMode::AutoFit,
            cell_margins: CellMargins::default(),
            borders: WordBorders::default(),
            shading_fill: None,
            look_first_row: true,
            look_last_row: false,
            look_first_col: true,
            look_last_col: false,
            look_h_band: true,
            look_v_band: false,
        }
    }
}

/// A table cell (`w:tc`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordTableCell {
    /// Preferred cell width (`w:tcW`).
    pub preferred_width: Option<TableWidth>,
    /// Horizontal grid span (`w:gridSpan`, defaults to `1`).
    pub grid_span: u16,
    /// Vertical merge state (`w:vMerge`).
    pub v_merge: VerticalMerge,
    /// Cell-specific padding overrides (`w:tcMar`).
    pub margins: Option<CellMargins>,
    /// Cell border overrides (`w:tcBorders`).
    pub borders: WordBorders,
    /// Cell background shading fill (`w:shd/@w:fill`, `#RRGGBB`).
    pub shading_fill: Option<String>,
    /// Vertical alignment of cell content (`w:vAlign`).
    pub vertical_align: PageVerticalAlign,
    /// Blocks inside the cell (paragraphs and/or nested tables).
    pub blocks: Vec<WordBlock>,
}

impl Default for WordTableCell {
    fn default() -> Self {
        Self {
            preferred_width: None,
            grid_span: 1,
            v_merge: VerticalMerge::None,
            margins: None,
            borders: WordBorders::default(),
            shading_fill: None,
            vertical_align: PageVerticalAlign::Top,
            blocks: Vec::new(),
        }
    }
}

/// A table row (`w:tr`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WordTableRow {
    /// Optional row height in points (`w:trHeight/@w:val`).
    pub height_pt: Option<f32>,
    /// Whether `height_pt` is an exact height (`w:hRule="exact"`) rather than `atLeast`.
    pub height_exact: bool,
    /// Repeat this row as a header at the top of each page (`w:tblHeader`).
    pub is_header: bool,
    /// Do not split this row across a page break (`w:cantSplit`).
    pub cant_split: bool,
    /// Cells in this row.
    pub cells: Vec<WordTableCell>,
}

/// A Word table (`w:tbl`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WordTable {
    /// Table properties (`w:tblPr`).
    pub props: TableProperties,
    /// Grid column widths in points (`w:tblGrid/w:gridCol/@w:w`).
    pub grid_col_widths_pt: Vec<f32>,
    /// Rows in the table.
    pub rows: Vec<WordTableRow>,
}

/// Block-level element in a section, header, footer, note, comment, or table cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum WordBlock {
    /// A paragraph (`w:p`).
    Paragraph(WordParagraph),
    /// A table (`w:tbl`).
    Table(WordTable),
}

/// A document section (`w:sectPr` + preceding blocks).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WordSection {
    /// Section page, column, margin, and header/footer properties.
    pub props: SectionProperties,
    /// Block-level content in this section.
    pub blocks: Vec<WordBlock>,
}

/// A header, footer, footnote, or endnote story (`Vec<WordBlock>`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WordStory {
    /// Identifier (relationship ID for headers/footers, integer ID string for notes).
    pub id: String,
    /// Blocks inside the story.
    pub blocks: Vec<WordBlock>,
}

/// A review comment (`word/comments.xml`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordComment {
    /// Comment ID (`w:id`).
    pub id: String,
    /// Author name (`w:author`).
    pub author: Option<String>,
    /// Author initials (`w:initials`).
    pub initials: Option<String>,
    /// Date string (`w:date`).
    pub date: Option<String>,
    /// Parent comment ID if this is a threaded reply (`w15:paraIdParent`).
    pub parent_id: Option<String>,
    /// Resolved/done flag (`w15:done`).
    pub done: bool,
    /// Comment body blocks.
    pub blocks: Vec<WordBlock>,
}

/// An external relationship or field reference recorded for forensic inspection (never fetched).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalReference {
    /// Relationship ID or field type.
    pub id: String,
    /// Relationship type URI or field name (`attachedTemplate`, `image`, `hyperlink`, `INCLUDEPICTURE`, etc.).
    pub ref_type: String,
    /// Target URI or file path.
    pub target: String,
}

/// Forensic & review indicators extracted from the Word document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForensicFacts {
    /// Whether a VBA macro project (`vbaProject.bin` or `Macros` storage) is present.
    pub vba_project_present: bool,
    /// Attached template path/URL (`w:attachedTemplate`), if any.
    pub attached_template: Option<String>,
    /// External relationships (`TargetMode="External"`) and external field targets.
    pub external_relationships: Vec<ExternalReference>,
    /// Document variables (`w:docVars/w:docVar`).
    pub document_variables: BTreeMap<String, String>,
    /// Custom document properties (`docProps/custom.xml`).
    pub custom_properties: BTreeMap<String, String>,
    /// Unique revision authors (`w:ins`, `w:del`, etc.).
    pub revision_authors: Vec<String>,
    /// Total revision count.
    pub revision_count: usize,
    /// Unique comment authors.
    pub comment_authors: Vec<String>,
    /// Number of runs marked hidden (`w:vanish` / `w:specVanish`).
    pub hidden_run_count: usize,
    /// Number of runs with tiny (`< 2 pt`) or white (`#FFFFFF`) text.
    pub concealed_run_count: usize,
    /// Embedded OLE / package object names.
    pub embedded_objects: Vec<String>,
}

/// Complete, format-independent Word document model produced by `prism_word::docx` and `prism_word::doc`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WordDocument {
    /// Document title (from `docProps/core.xml` or filename fallback).
    pub title: Option<String>,
    /// Document author / creator.
    pub author: Option<String>,
    /// Core and extended metadata properties.
    pub properties: BTreeMap<String, String>,
    /// Document-level settings and compatibility flags.
    pub settings: WordSettings,
    /// Office theme (`word/theme/theme1.xml`).
    pub theme: WordTheme,
    /// Style definitions (`word/styles.xml` or `STSH`).
    pub styles: WordStyleSheet,
    /// Numbering definitions (`word/numbering.xml` or `PlfLst`/`PlfLfo`).
    pub numbering: WordNumbering,
    /// Ordered sections (`w:sectPr` + blocks).
    pub sections: Vec<WordSection>,
    /// Header stories keyed by relationship ID.
    pub headers: BTreeMap<String, WordStory>,
    /// Footer stories keyed by relationship ID.
    pub footers: BTreeMap<String, WordStory>,
    /// Footnotes keyed by note ID.
    pub footnotes: BTreeMap<i32, WordStory>,
    /// Endnotes keyed by note ID.
    pub endnotes: BTreeMap<i32, WordStory>,
    /// Comments keyed by comment ID.
    pub comments: BTreeMap<String, WordComment>,
    /// Stories whose placement the reader does not decode yet (legacy `.doc` footnote, header,
    /// annotation, endnote and text box text). Kept so extracted text stays complete; they are
    /// not drawn by the layout engine.
    #[serde(default)]
    pub unplaced_stories: Vec<WordStory>,
    /// Embedded images.
    pub images: Vec<ImageResource>,
    /// Embedded (and deobfuscated) fonts.
    pub fonts: Vec<FontResource>,
    /// Forensic indicators and external references.
    pub forensic: ForensicFacts,
}

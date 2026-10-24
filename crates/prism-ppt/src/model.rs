// SPDX-License-Identifier: AGPL-3.0-only
//! Rich `PowerPoint` presentation model (`Presentation`) shared by the `.pptx` and `.ppt` readers.

use prism_core::document::{FontResource, ImageResource};
use prism_drawingml::{ChartSpace, CustomGeometry, DrawingTheme};
use prism_layout::Rgba;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Default 16:9 widescreen slide width in points (`13.333 in * 72 = 960 pt`).
pub const DEFAULT_SLIDE_WIDTH_PT: f32 = 960.0;
/// Default slide height in points (`7.5 in * 72 = 540 pt`).
pub const DEFAULT_SLIDE_HEIGHT_PT: f32 = 540.0;

/// Forensic facts recorded during `.pptx` / `.ppt` parsing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresentationForensicFacts {
    /// Whether a VBA macro project (`ppt/vbaProject.bin` or `VBA` storage) is present.
    pub vba_project_present: bool,
    /// External relationships or action links (`TargetMode="External"`).
    pub external_links: Vec<String>,
    /// Embedded or linked audio/video media targets.
    pub media_objects: Vec<String>,
    /// 1-based slide numbers of hidden slides (`p:sld/@show="0"` or `SSSlideInfoAtom` hidden bit).
    pub hidden_slide_numbers: Vec<u32>,
    /// Number of shapes marked hidden (`p:cNvPr/@hidden="1"`).
    pub hidden_shape_count: usize,
    /// Number of shapes positioned completely outside the slide bounds.
    pub off_slide_shape_count: usize,
    /// Distinct slide comment authors in deterministic order.
    pub comment_authors: Vec<String>,
}

/// Placeholder reference linking a slide shape to its layout and master placeholder (`<p:ph>`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaceholderRef {
    /// Placeholder type (`title`, `ctrTitle`, `subTitle`, `body`, `obj`, `dt`, `ftr`, `sldNum`, etc.).
    pub ph_type: Option<String>,
    /// Placeholder index (`@idx`).
    pub idx: Option<u32>,
}

impl PlaceholderRef {
    /// Check whether `self` matches a candidate placeholder on a layout or master.
    #[must_use]
    pub fn matches(&self, other: &Self) -> bool {
        if let (Some(i1), Some(i2)) = (self.idx, other.idx)
            && i1 == i2
        {
            return true;
        }
        if let (Some(t1), Some(t2)) = (self.ph_type.as_deref(), other.ph_type.as_deref()) {
            if t1.eq_ignore_ascii_case(t2) {
                return true;
            }
            // ctrTitle / title and subTitle / body fallback matching in OOXML
            if (matches!(t1, "ctrTitle" | "title") && matches!(t2, "ctrTitle" | "title"))
                || (matches!(t1, "subTitle" | "body" | "obj")
                    && matches!(t2, "subTitle" | "body" | "obj"))
            {
                return true;
            }
        }
        false
    }
}

/// Fill style for a shape or slide background.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum ShapeFill {
    /// Inherit from layout/master or default (`None`).
    #[default]
    Inherited,
    /// Explicit no-fill (`a:noFill`).
    NoFill,
    /// Solid RGBA color (`a:solidFill`).
    Solid(Rgba),
    /// Linear gradient (`a:gradFill`).
    LinearGradient {
        /// Angle in degrees (`0` = left-to-right, `90` = top-to-bottom).
        angle_deg: f32,
        /// Gradient stops `(offset 0.0..=1.0, Rgba)`.
        stops: Vec<(f32, Rgba)>,
    },
    /// Picture fill (`a:blipFill`).
    Picture {
        /// Image resource ID.
        resource_id: String,
    },
}

/// Stroke style for a shape or table border (`a:ln`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShapeStroke {
    /// Stroke color.
    pub color: Rgba,
    /// Stroke width in points.
    pub width_pt: f32,
    /// Dash pattern (`solid`, `dash`, `dot`, `sysDot`, `dashDot`).
    pub dash: Option<String>,
}

/// Text autofit mode (`a:noAutofit`, `a:normAutofit`, `a:spAutoFit`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub enum AutofitMode {
    /// Do not autofit (`a:noAutofit`).
    #[default]
    None,
    /// Shrink text on overflow (`a:normAutofit`).
    Normal {
        /// Stored font scale percentage (`10.0..=100.0`, default `100.0`).
        font_scale_pct: f32,
        /// Stored line-spacing reduction percentage (`0.0..=90.0`, default `0.0`).
        line_space_reduction_pct: f32,
    },
    /// Resize shape height to fit text (`a:spAutoFit`).
    Shape,
}

/// Vertical alignment inside a text box or table cell (`a:bodyPr/@anchor`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextVerticalAnchor {
    /// Top (`t`).
    #[default]
    Top,
    /// Middle / Center (`ctr`).
    Middle,
    /// Bottom (`b`).
    Bottom,
}

/// Horizontal text alignment (`a:pPr/@algn`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PptTextAlign {
    /// Left (`l`).
    #[default]
    Left,
    /// Center (`ctr`).
    Center,
    /// Right (`r`).
    Right,
    /// Justify (`just`).
    Justify,
}

/// Bullet specification on a paragraph (`a:buNone`, `a:buChar`, `a:buAutoNum`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BulletKind {
    /// Inherit from list style.
    #[default]
    Inherited,
    /// Explicitly no bullet (`a:buNone`).
    None,
    /// Character bullet (`a:buChar/@char`).
    Char(String),
    /// Auto-numbered bullet (`a:buAutoNum/@type`, `@startAt`).
    AutoNum {
        /// Numbering scheme (e.g. `arabicPeriod`, `romanUcPeriod`, `alphaLcPeriod`).
        scheme: String,
        /// Starting number (1-based).
        start_at: u32,
    },
}

/// Paragraph-level properties in `DrawingML` text (`a:pPr`, `a:lvl1pPr..a:lvl9pPr`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TextLevelStyle {
    /// Zero-based indent level (`0..=8`).
    pub level: u8,
    /// Horizontal alignment.
    pub align: Option<PptTextAlign>,
    /// Left margin in points (`@marL`).
    pub margin_left_pt: Option<f32>,
    /// First-line indent in points (`@indent`).
    pub indent_pt: Option<f32>,
    /// Line spacing multiplier (`1.0` = single) or exact points (negative means exact points).
    pub line_spacing_mult: Option<f32>,
    /// Space before paragraph in points (`a:spcBef`).
    pub space_before_pt: Option<f32>,
    /// Space after paragraph in points (`a:spcAft`).
    pub space_after_pt: Option<f32>,
    /// Bullet style.
    pub bullet: BulletKind,
    /// Default run properties (`a:defRPr`).
    pub default_run: TextRunStyle,
}

/// List style with up to 9 levels (`a:lstStyle`, `p:titleStyle`, `p:bodyStyle`, `p:otherStyle`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TextListStyle {
    /// Styles by level (`0..=8`).
    pub levels: BTreeMap<u8, TextLevelStyle>,
}

/// Run-level text formatting (`a:rPr`, `a:defRPr`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TextRunStyle {
    /// Font family (`a:latin/@typeface`).
    pub font_family: Option<String>,
    /// Font size in points (`@sz` in hundredths of a point).
    pub font_size_pt: Option<f32>,
    /// Bold (`@b`).
    pub bold: Option<bool>,
    /// Italic (`@i`).
    pub italic: Option<bool>,
    /// Underline (`@u`).
    pub underline: Option<bool>,
    /// Strikethrough (`@strike`).
    pub strikethrough: Option<bool>,
    /// Baseline offset percentage (`> 0` superscript, `< 0` subscript).
    pub baseline_pct: Option<f32>,
    /// Text fill color.
    pub color: Option<Rgba>,
    /// Hyperlink target (`a:hlinkClick`).
    pub hyperlink: Option<String>,
}

/// A run of text inside a [`TextParagraph`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PptTextRun {
    /// UTF-8 text content.
    pub text: String,
    /// Run formatting.
    pub style: TextRunStyle,
    /// Optional dynamic field type (`slidenum`, `datetime`).
    pub field_type: Option<String>,
}

/// A paragraph inside a [`TextBody`] (`a:p`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TextParagraph {
    /// Paragraph properties (`a:pPr`).
    pub props: TextLevelStyle,
    /// Runs in reading order.
    pub runs: Vec<PptTextRun>,
}

/// A `DrawingML` text body (`p:txBody` / `a:txBody`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextBody {
    /// Left inset in points (`@lIns`, default `7.2 pt`).
    pub inset_left_pt: f32,
    /// Top inset in points (`@tIns`, default `3.6 pt`).
    pub inset_top_pt: f32,
    /// Right inset in points (`@rIns`, default `7.2 pt`).
    pub inset_right_pt: f32,
    /// Bottom inset in points (`@bIns`, default `3.6 pt`).
    pub inset_bottom_pt: f32,
    /// Whether text wraps inside the shape (`@wrap != "none"`).
    pub word_wrap: bool,
    /// Vertical anchor (`@anchor`).
    pub vertical_anchor: TextVerticalAnchor,
    /// Number of text columns (`@numCol`, default `1`).
    pub num_cols: u8,
    /// Column spacing in points (`@spcCol`).
    pub col_spacing_pt: f32,
    /// Text autofit mode (`a:noAutofit`, `a:normAutofit`, `a:spAutoFit`).
    pub autofit: AutofitMode,
    /// Local list style overrides (`a:lstStyle`).
    pub list_style: TextListStyle,
    /// Paragraphs (`a:p`).
    pub paragraphs: Vec<TextParagraph>,
}

impl Default for TextBody {
    fn default() -> Self {
        Self {
            inset_left_pt: 7.2,
            inset_top_pt: 3.6,
            inset_right_pt: 7.2,
            inset_bottom_pt: 3.6,
            word_wrap: true,
            vertical_anchor: TextVerticalAnchor::Top,
            num_cols: 1,
            col_spacing_pt: 0.0,
            autofit: AutofitMode::None,
            list_style: TextListStyle::default(),
            paragraphs: Vec::new(),
        }
    }
}

/// A cell inside a [`SlideTable`] (`a:tc`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SlideTableCell {
    /// Text content of the cell (`a:txBody`).
    pub text_body: Option<TextBody>,
    /// Horizontal span (`@gridSpan`, default `1`).
    pub grid_span: u16,
    /// Vertical span (`@rowSpan`, default `1`).
    pub row_span: u16,
    /// Covered by horizontal merge (`@hMerge="1"`).
    pub h_merge: bool,
    /// Covered by vertical merge (`@vMerge="1"`).
    pub v_merge: bool,
    /// Cell fill override (`a:tcPr`).
    pub fill: ShapeFill,
    /// Top border (`a:lnT`).
    pub border_top: Option<ShapeStroke>,
    /// Bottom border (`a:lnB`).
    pub border_bottom: Option<ShapeStroke>,
    /// Left border (`a:lnL`).
    pub border_left: Option<ShapeStroke>,
    /// Right border (`a:lnR`).
    pub border_right: Option<ShapeStroke>,
}

/// A row inside a [`SlideTable`] (`a:tr`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SlideTableRow {
    /// Minimum row height in points (`@h`).
    pub height_pt: f32,
    /// Cells in this row.
    pub cells: Vec<SlideTableCell>,
}

/// A `DrawingML` table on a slide (`a:tbl`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SlideTable {
    /// Grid column widths in points (`a:tblGrid/a:gridCol/@w`).
    pub col_widths_pt: Vec<f32>,
    /// Table rows.
    pub rows: Vec<SlideTableRow>,
    /// Whether the first row uses header styling (`@firstRow="1"`).
    pub first_row: bool,
    /// Whether alternating rows are banded (`@bandRow="1"`).
    pub band_row: bool,
    /// Table style GUID (`a:tableStyleId`).
    pub style_id: Option<String>,
}

/// Payload variant of a [`SlideShape`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ShapeKind {
    /// Standard shape or text box (`p:sp`).
    AutoShape {
        /// Optional text body (`p:txBody`).
        text_body: Option<TextBody>,
    },
    /// Picture shape (`p:pic`).
    Picture {
        /// Image resource ID.
        resource_id: String,
        /// Crop fractions `(left, top, right, bottom)` in `0.0..=1.0`.
        crop: Option<(f32, f32, f32, f32)>,
        /// Opacity (`0.0..=1.0`).
        opacity: f32,
    },
    /// Grouped shapes (`p:grpSp`).
    Group {
        /// Child coordinate space origin X in points (`a:chOff/@x`).
        ch_off_x_pt: f32,
        /// Child coordinate space origin Y in points (`a:chOff/@y`).
        ch_off_y_pt: f32,
        /// Child coordinate space width in points (`a:chExt/@cx`).
        ch_ext_w_pt: f32,
        /// Child coordinate space height in points (`a:chExt/@cy`).
        ch_ext_h_pt: f32,
        /// Child shapes.
        children: Vec<SlideShape>,
    },
    /// `DrawingML` table (`p:graphicFrame` containing `a:tbl`).
    Table(SlideTable),
    /// Native `DrawingML` chart (`p:graphicFrame` containing `c:chart` / `cx:chart`).
    Chart(Box<ChartSpace>),
    /// `SmartArt` diagram (`p:graphicFrame` containing `dgm:relIds`).
    SmartArt {
        /// Pre-rendered shapes from `ppt/diagrams/drawing*.xml` (`dsp:drawing`), if present.
        shapes: Vec<SlideShape>,
        /// Plain-text items extracted from `ppt/diagrams/data*.xml` (`dgm:dataModel`) as fallback.
        fallback_items: Vec<String>,
    },
    /// Audio or video media object rendered as a static poster frame (`p:pic` / `p14:media`).
    MediaPoster {
        /// Optional poster image resource ID.
        poster_resource_id: Option<String>,
        /// Media file target name.
        media_target: String,
        /// Display label.
        label: String,
    },
    /// Connector line (`p:cxnSp`).
    Connector {
        /// Whether the start point has an arrowhead (`a:headEnd`).
        head_arrow: bool,
        /// Whether the end point has an arrowhead (`a:tailEnd`).
        tail_arrow: bool,
    },
}

/// A positioned shape on a slide, layout, or master.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct SlideShape {
    /// Shape identifier (`p:cNvPr/@id`).
    pub id: String,
    /// Shape name (`p:cNvPr/@name`).
    pub name: String,
    /// Left X position in points (`a:off/@x`).
    pub x_pt: f32,
    /// Top Y position in points (`a:off/@y`).
    pub y_pt: f32,
    /// Width in points (`a:ext/@cx`).
    pub width_pt: f32,
    /// Height in points (`a:ext/@cy`).
    pub height_pt: f32,
    /// Whether an explicit `a:xfrm` was present on this shape (if `false`, inherit from placeholder).
    pub has_explicit_xfrm: bool,
    /// Clockwise rotation in degrees (`a:xfrm/@rot` / 60000).
    pub rotation_deg: f32,
    /// Horizontal flip (`a:xfrm/@flipH="1"`).
    pub flip_h: bool,
    /// Vertical flip (`a:xfrm/@flipV="1"`).
    pub flip_v: bool,
    /// Hidden shape (`p:cNvPr/@hidden="1"`).
    pub hidden: bool,
    /// Optional placeholder link (`p:nvPr/p:ph`).
    pub placeholder: Option<PlaceholderRef>,
    /// Preset geometry name (`a:prstGeom/@prst`, e.g. `"rect"`, `"roundRect"`).
    pub preset_geom: Option<String>,
    /// Adjust values (`a:avLst/a:gd`).
    pub adjust_values: BTreeMap<String, i64>,
    /// Custom geometry (`a:custGeom`).
    pub custom_geom: Option<CustomGeometry>,
    /// Shape fill.
    pub fill: ShapeFill,
    /// Shape outline stroke.
    pub stroke: Option<ShapeStroke>,
    /// Inert hyperlink (`a:hlinkClick`).
    pub hyperlink: Option<String>,
    /// Alternative text (`p:cNvPr/@descr`).
    pub alt_text: Option<String>,
    /// Specific shape payload.
    pub kind: ShapeKind,
}

impl Default for SlideShape {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            x_pt: 0.0,
            y_pt: 0.0,
            width_pt: 0.0,
            height_pt: 0.0,
            has_explicit_xfrm: false,
            rotation_deg: 0.0,
            flip_h: false,
            flip_v: false,
            hidden: false,
            placeholder: None,
            preset_geom: None,
            adjust_values: BTreeMap::new(),
            custom_geom: None,
            fill: ShapeFill::Inherited,
            stroke: None,
            hyperlink: None,
            alt_text: None,
            kind: ShapeKind::AutoShape { text_body: None },
        }
    }
}

/// Speaker notes attached to a slide (`ppt/notesSlides/notesSlide*.xml` or `NotesContainer`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NotesSlide {
    /// Shapes on the notes page (typically the slide image placeholder and the notes text body).
    pub shapes: Vec<SlideShape>,
    /// Extracted plain text of the speaker notes.
    pub plain_text: String,
}

/// Reviewer comment on a slide (`ppt/comments/comment*.xml` or `p188:cmLst`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SlideComment {
    /// Author display name.
    pub author: String,
    /// Author initials.
    pub initials: String,
    /// X position on the slide in points.
    pub x_pt: f32,
    /// Y position on the slide in points.
    pub y_pt: f32,
    /// Comment text.
    pub text: String,
    /// Optional timestamp string (`@dt`, stored only, never used to vary output).
    pub date: Option<String>,
}

/// Slide master (`ppt/slideMasters/slideMaster*.xml` or `MainMasterContainer`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SlideMaster {
    /// Master identifier / path.
    pub id: String,
    /// Associated theme path (`ppt/theme/theme*.xml`).
    pub theme_id: Option<String>,
    /// Master background (`p:bg`).
    pub background: ShapeFill,
    /// Color map (`p:clrMap`: `bg1` -> `lt1`, `tx1` -> `dk1`, etc.).
    pub color_map: BTreeMap<String, String>,
    /// Shapes on the master (both decorative background shapes and placeholder templates).
    pub shapes: Vec<SlideShape>,
    /// Master title text style (`p:txStyles/p:titleStyle`).
    pub title_style: TextListStyle,
    /// Master body text style (`p:txStyles/p:bodyStyle`).
    pub body_style: TextListStyle,
    /// Master other text style (`p:txStyles/p:otherStyle`).
    pub other_style: TextListStyle,
}

/// Slide layout (`ppt/slideLayouts/slideLayout*.xml`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlideLayout {
    /// Layout identifier / path.
    pub id: String,
    /// Parent slide master identifier / path.
    pub master_id: Option<String>,
    /// Whether decorative shapes from the master are shown (`@showMasterSp`, default `true`).
    pub show_master_shapes: bool,
    /// Layout background override (`p:bg`).
    pub background: ShapeFill,
    /// Shapes on the layout.
    pub shapes: Vec<SlideShape>,
}

impl Default for SlideLayout {
    fn default() -> Self {
        Self {
            id: String::new(),
            master_id: None,
            show_master_shapes: true,
            background: ShapeFill::Inherited,
            shapes: Vec::new(),
        }
    }
}

/// A single slide in a [`Presentation`] (`ppt/slides/slide*.xml` or `SlideContainer`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Slide {
    /// 1-based slide number in presentation order.
    pub number: u32,
    /// Slide identifier / path.
    pub id: String,
    /// Parent layout identifier / path.
    pub layout_id: Option<String>,
    /// Direct master identifier (used by legacy `.ppt` slides or when layout is absent).
    pub master_id: Option<String>,
    /// Whether decorative shapes from the layout and master are shown (`@showMasterSp`, default `true`).
    pub show_master_shapes: bool,
    /// Whether this slide is hidden in slide show mode (`@show="0"`).
    pub hidden: bool,
    /// Slide background override (`p:bg`).
    pub background: ShapeFill,
    /// Shapes on this slide in back-to-front Z-order.
    pub shapes: Vec<SlideShape>,
    /// Optional speaker notes.
    pub notes: Option<NotesSlide>,
    /// Reviewer comments attached to this slide.
    pub comments: Vec<SlideComment>,
}

impl Default for Slide {
    fn default() -> Self {
        Self {
            number: 1,
            id: String::new(),
            layout_id: None,
            master_id: None,
            show_master_shapes: true,
            hidden: false,
            background: ShapeFill::Inherited,
            shapes: Vec::new(),
            notes: None,
            comments: Vec::new(),
        }
    }
}

/// Complete `PowerPoint` presentation model (`Presentation`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Presentation {
    /// Slide width in points (`p:sldSz/@cx`).
    pub slide_width_pt: f32,
    /// Slide height in points (`p:sldSz/@cy`).
    pub slide_height_pt: f32,
    /// Notes page width in points (`p:notesSz/@cx`, default `540 pt`).
    pub notes_width_pt: f32,
    /// Notes page height in points (`p:notesSz/@cy`, default `720 pt`).
    pub notes_height_pt: f32,
    /// Document title (`dc:title`).
    pub title: Option<String>,
    /// Document author (`dc:creator`).
    pub author: Option<String>,
    /// Themes keyed by part path or ID.
    pub themes: BTreeMap<String, DrawingTheme>,
    /// Slide masters keyed by part path or ID.
    pub masters: BTreeMap<String, SlideMaster>,
    /// Slide layouts keyed by part path or ID.
    pub layouts: BTreeMap<String, SlideLayout>,
    /// Slides in presentation order.
    pub slides: Vec<Slide>,
    /// Embedded raster/vector images keyed by resource ID.
    pub images: BTreeMap<String, ImageResource>,
    /// Embedded fonts keyed by family name.
    pub fonts: BTreeMap<String, FontResource>,
    /// Forensic facts recorded during parsing.
    pub forensic: PresentationForensicFacts,
}

impl Default for Presentation {
    fn default() -> Self {
        Self {
            slide_width_pt: DEFAULT_SLIDE_WIDTH_PT,
            slide_height_pt: DEFAULT_SLIDE_HEIGHT_PT,
            notes_width_pt: 540.0,
            notes_height_pt: 720.0,
            title: None,
            author: None,
            themes: BTreeMap::new(),
            masters: BTreeMap::new(),
            layouts: BTreeMap::new(),
            slides: Vec::new(),
            images: BTreeMap::new(),
            fonts: BTreeMap::new(),
            forensic: PresentationForensicFacts::default(),
        }
    }
}

impl Presentation {
    /// Resolve the `(Option<&SlideLayout>, Option<&SlideMaster>, &DrawingTheme)` chain for `slide`.
    #[must_use]
    pub fn resolve_slide_hierarchy<'a>(
        &'a self,
        slide: &'a Slide,
        fallback_theme: &'a DrawingTheme,
    ) -> (
        Option<&'a SlideLayout>,
        Option<&'a SlideMaster>,
        &'a DrawingTheme,
    ) {
        let layout = slide
            .layout_id
            .as_deref()
            .and_then(|lid| self.layouts.get(lid));
        let master = layout
            .and_then(|l| l.master_id.as_deref())
            .or(slide.master_id.as_deref())
            .and_then(|mid| self.masters.get(mid))
            .or_else(|| self.masters.values().next());
        let theme = master
            .and_then(|m| m.theme_id.as_deref())
            .and_then(|tid| self.themes.get(tid))
            .or_else(|| self.themes.values().next())
            .unwrap_or(fallback_theme);
        (layout, master, theme)
    }

    /// Resolve a placeholder shape across `layout` and `master` matching `ph`.
    #[must_use]
    pub fn resolve_placeholder_chain<'a>(
        ph: &PlaceholderRef,
        layout: Option<&'a SlideLayout>,
        master: Option<&'a SlideMaster>,
    ) -> (Option<&'a SlideShape>, Option<&'a SlideShape>) {
        let layout_ph = layout.and_then(|l| {
            l.shapes
                .iter()
                .find(|s| s.placeholder.as_ref().is_some_and(|p| ph.matches(p)))
        });
        let master_ph = master.and_then(|m| {
            m.shapes
                .iter()
                .find(|s| s.placeholder.as_ref().is_some_and(|p| ph.matches(p)))
        });
        (layout_ph, master_ph)
    }
}

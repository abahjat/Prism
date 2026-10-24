// SPDX-License-Identifier: AGPL-3.0-only
//! Native Rust positioned layout core for Prism (`prism-layout`).
//!
//! `prism-layout` defines the canonical positioned page model ([`LayoutDocument`], [`LayoutPage`],
//! [`Item`], [`GlyphRun`]), the hermetic font discovery and substitution engine ([`FontSystem`],
//! [`FontReport`]), `.odttf` embedded font deobfuscation ([`deobfuscate_odttf`]), rich-text
//! paragraph shaping ([`TextLayoutEngine`]), and reading-order text extraction, character boxes,
//! and search ([`LayoutPage::text`], [`LayoutPage::char_boxes`], [`LayoutDocument::find`]).

#![warn(missing_docs)]
#![warn(clippy::all)]
#![warn(clippy::pedantic)]

pub mod engine;
pub mod fonts;
pub mod model;
pub mod pdf_input;
pub mod text_ops;
pub mod udm;

pub use engine::{
    LineSpacing, ParagraphSpec, ScriptPosition, ShapedLine, ShapedParagraph, SpanSpec, TextAlign,
    TextLayoutEngine,
};
pub use fonts::{
    FontSystem, deobfuscate_odttf, normalize_family_key, strip_subset_prefix,
    substitute_font_family,
};
pub use model::{
    Affine, AnchorItem, ClipPath, Cluster, Decorations, FillPaint, FillRule, FontEntry, FontId,
    FontReport, FontResolutionEntry, FontSourceKind, FontStore, GlyphRun, GradientStop, GroupItem,
    ImageEntry, ImageId, ImageItem, ImageStore, Item, LayoutDocument, LayoutOptions, LayoutPage,
    LineCap, LineDecoration, LineJoin, LinearGradient, LinkItem, MarkupMode, NORMALIZED_UPEM,
    PathCommand, PathItem, Point, PositionedGlyph, RadialGradient, Rect, Rgba, Size, SourceRef,
    StrokeStyle, normalize_zero,
};
pub use pdf_input::{interpret_pdf_bytes, interpret_pdf_pages};
pub use text_ops::{CharBox, SearchHit, SearchOptions};
pub use udm::{layout_document, layout_document_paginated, layout_document_paginated_range};

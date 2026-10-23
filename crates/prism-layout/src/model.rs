// SPDX-License-Identifier: AGPL-3.0-only
//! Positioned page model for `prism-layout`.
//!
//! All coordinates and dimensions are expressed in typographic points (`1 pt = 1/72 in`),
//! with the origin `(0, 0)` at the top-left corner of the page and `+y` pointing downward.
//! Z-order is defined by the slice order of [`LayoutPage::items`].

use prism_core::error::{Error, Result};
use serde::{Deserialize, Serialize};
use skrifa::MetadataProvider;
use skrifa::instance::{LocationRef, Size as SkrifaSize};
use skrifa::outline::{DrawSettings, OutlinePen};
use std::collections::BTreeMap;
use std::ops::Range;

/// Canonical units-per-em used for normalized glyph outlines in `<defs>` and caches.
pub const NORMALIZED_UPEM: f32 = 1000.0;

/// Resource limits and configuration options for layout and interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutOptions {
    /// Maximum number of pages permitted in a single [`LayoutDocument`].
    pub max_pages: usize,
    /// Maximum number of positioned glyphs permitted on a single [`LayoutPage`].
    pub max_glyphs_per_page: usize,
    /// Maximum number of pixels (`width * height`) permitted for a single image or raster surface.
    pub max_image_pixels: u64,
    /// Maximum decompressed stream size in bytes.
    pub max_decompressed_stream_bytes: usize,
    /// Maximum nesting depth for groups or nested form `XObjects`.
    pub max_recursion_depth: u32,
    /// Whether host system fonts may be queried when a font is neither embedded nor bundled.
    ///
    /// Defaults to `false` for forensic determinism across machines.
    pub allow_system_fonts: bool,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            max_pages: 10_000,
            max_glyphs_per_page: 250_000,
            max_image_pixels: 100_000_000,                   // 100 MP
            max_decompressed_stream_bytes: 64 * 1024 * 1024, // 64 MiB
            max_recursion_depth: 32,
            allow_system_fonts: false,
        }
    }
}

/// A 2D size in points.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Size {
    /// Width in points.
    pub width: f32,
    /// Height in points.
    pub height: f32,
}

impl Size {
    /// US Letter page size (`612 × 792 pt`).
    pub const LETTER: Self = Self {
        width: 612.0,
        height: 792.0,
    };

    /// ISO A4 page size (`595.28 × 841.89 pt`).
    pub const A4: Self = Self {
        width: 595.28,
        height: 841.89,
    };

    /// Construct a new [`Size`].
    #[must_use]
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

/// A 2D point in points (origin top-left, `+y` down).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    /// Horizontal coordinate in points.
    pub x: f32,
    /// Vertical coordinate in points.
    pub y: f32,
}

impl Point {
    /// Origin `(0, 0)`.
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    /// Construct a new [`Point`].
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// An axis-aligned rectangle in points (origin top-left, `+y` down).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    /// Left X coordinate in points.
    pub x: f32,
    /// Top Y coordinate in points.
    pub y: f32,
    /// Width in points.
    pub width: f32,
    /// Height in points.
    pub height: f32,
}

impl Rect {
    /// Zero rectangle at the origin.
    pub const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
    };

    /// Construct a new [`Rect`].
    #[must_use]
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Right edge (`x + width`).
    #[must_use]
    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    /// Bottom edge (`y + height`).
    #[must_use]
    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    /// Check whether this rectangle contains `point`.
    #[must_use]
    pub fn contains(&self, point: Point) -> bool {
        point.x >= self.x
            && point.x <= self.right()
            && point.y >= self.y
            && point.y <= self.bottom()
    }

    /// Compute the bounding union of two rectangles.
    #[must_use]
    pub fn union(&self, other: Self) -> Self {
        let min_x = self.x.min(other.x);
        let min_y = self.y.min(other.y);
        let max_x = self.right().max(other.right());
        let max_y = self.bottom().max(other.bottom());
        Self::new(
            min_x,
            min_y,
            (max_x - min_x).max(0.0),
            (max_y - min_y).max(0.0),
        )
    }
}

/// A 2D affine transformation matrix `[a, b, c, d, tx, ty]`.
///
/// Transforms `(x, y)` to `(a*x + c*y + tx, b*x + d*y + ty)`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Affine {
    /// Matrix coefficients `[a, b, c, d, tx, ty]`.
    pub coeffs: [f32; 6],
}

impl Default for Affine {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Affine {
    /// Identity transform `[1, 0, 0, 1, 0, 0]`.
    pub const IDENTITY: Self = Self {
        coeffs: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
    };

    /// Construct an [`Affine`] from `[a, b, c, d, tx, ty]`.
    #[must_use]
    pub const fn new(coeffs: [f32; 6]) -> Self {
        Self { coeffs }
    }

    /// Translation by `(tx, ty)`.
    #[must_use]
    pub const fn translate(tx: f32, ty: f32) -> Self {
        Self {
            coeffs: [1.0, 0.0, 0.0, 1.0, tx, ty],
        }
    }

    /// Uniform scale by `s`.
    #[must_use]
    pub const fn scale(s: f32) -> Self {
        Self {
            coeffs: [s, 0.0, 0.0, s, 0.0, 0.0],
        }
    }

    /// Non-uniform scale by `(sx, sy)`.
    #[must_use]
    pub const fn scale_non_uniform(sx: f32, sy: f32) -> Self {
        Self {
            coeffs: [sx, 0.0, 0.0, sy, 0.0, 0.0],
        }
    }

    /// Clockwise rotation by `degrees` in top-left (`+y` down) coordinates.
    #[must_use]
    pub fn rotate_deg(degrees: f32) -> Self {
        let rad = degrees.to_radians();
        let (sin, cos) = rad.sin_cos();
        Self {
            coeffs: [cos, sin, -sin, cos, 0.0, 0.0],
        }
    }

    /// Clockwise rotation by `degrees` around `center`.
    #[must_use]
    pub fn rotate_around(degrees: f32, center: Point) -> Self {
        Self::translate(center.x, center.y)
            .then(Self::rotate_deg(degrees))
            .then(Self::translate(-center.x, -center.y))
    }

    /// Return `true` if this transform is the exact identity matrix.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        let [a, b, c, d, tx, ty] = self.coeffs;
        (a - 1.0).abs() <= 1e-6
            && b.abs() <= 1e-6
            && c.abs() <= 1e-6
            && (d - 1.0).abs() <= 1e-6
            && tx.abs() <= 1e-6
            && ty.abs() <= 1e-6
    }

    /// Concatenate `self * other` (apply `other` first, then `self`).
    #[must_use]
    pub fn then(self, other: Self) -> Self {
        let [a1, b1, c1, d1, tx1, ty1] = self.coeffs;
        let [a2, b2, c2, d2, tx2, ty2] = other.coeffs;
        Self {
            coeffs: [
                a1 * a2 + c1 * b2,
                b1 * a2 + d1 * b2,
                a1 * c2 + c1 * d2,
                b1 * c2 + d1 * d2,
                a1 * tx2 + c1 * ty2 + tx1,
                b1 * tx2 + d1 * ty2 + ty1,
            ],
        }
    }

    /// Transform a 2D [`Point`].
    #[must_use]
    pub fn transform_point(&self, point: Point) -> Point {
        let [a, b, c, d, tx, ty] = self.coeffs;
        Point::new(
            a * point.x + c * point.y + tx,
            b * point.x + d * point.y + ty,
        )
    }

    /// Compute the axis-aligned bounding box of `rect` after applying `self`.
    #[must_use]
    pub fn transform_rect_bbox(&self, rect: Rect) -> Rect {
        let corners = [
            self.transform_point(Point::new(rect.x, rect.y)),
            self.transform_point(Point::new(rect.right(), rect.y)),
            self.transform_point(Point::new(rect.x, rect.bottom())),
            self.transform_point(Point::new(rect.right(), rect.bottom())),
        ];
        let mut min_x = corners[0].x;
        let mut max_x = corners[0].x;
        let mut min_y = corners[0].y;
        let mut max_y = corners[0].y;
        for c in &corners[1..] {
            min_x = min_x.min(c.x);
            max_x = max_x.max(c.x);
            min_y = min_y.min(c.y);
            max_y = max_y.max(c.y);
        }
        Rect::new(
            min_x,
            min_y,
            (max_x - min_x).max(0.0),
            (max_y - min_y).max(0.0),
        )
    }
}

/// An 8-bit sRGB color with alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Rgba {
    /// Red channel (`0..=255`).
    pub r: u8,
    /// Green channel (`0..=255`).
    pub g: u8,
    /// Blue channel (`0..=255`).
    pub b: u8,
    /// Alpha channel (`0..=255`, where `255` is opaque).
    pub a: u8,
}

impl Default for Rgba {
    fn default() -> Self {
        Self::BLACK
    }
}

impl Rgba {
    /// Opaque black (`#000000`).
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    /// Opaque white (`#FFFFFF`).
    pub const WHITE: Self = Self::rgb(255, 255, 255);
    /// Fully transparent (`rgba(0, 0, 0, 0)`).
    pub const TRANSPARENT: Self = Self::new(0, 0, 0, 0);

    /// Construct an [`Rgba`] color.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Construct an opaque [`Rgba`] color (`a = 255`).
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Parse a CSS color string (`#RGB`, `#RRGGBB`, `#RRGGBBAA`, `rgb(...)`, `rgba(...)`, or basic named color).
    #[must_use]
    pub fn from_css_str(input: &str) -> Option<Self> {
        let trimmed = input.trim();
        if let Some(hex) = trimmed.strip_prefix('#') {
            return match hex.len() {
                3 => {
                    let red = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
                    let green = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
                    let blue = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
                    Some(Self::rgb(red, green, blue))
                }
                6 => {
                    let red = u8::from_str_radix(&hex[0..2], 16).ok()?;
                    let green = u8::from_str_radix(&hex[2..4], 16).ok()?;
                    let blue = u8::from_str_radix(&hex[4..6], 16).ok()?;
                    Some(Self::rgb(red, green, blue))
                }
                8 => {
                    let red = u8::from_str_radix(&hex[0..2], 16).ok()?;
                    let green = u8::from_str_radix(&hex[2..4], 16).ok()?;
                    let blue = u8::from_str_radix(&hex[4..6], 16).ok()?;
                    let alpha = u8::from_str_radix(&hex[6..8], 16).ok()?;
                    Some(Self::new(red, green, blue, alpha))
                }
                _ => None,
            };
        }

        let lower = trimmed.to_ascii_lowercase();
        if let Some(inner) = lower
            .strip_prefix("rgba(")
            .or_else(|| lower.strip_prefix("rgb("))
            .and_then(|v| v.strip_suffix(')'))
        {
            let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
            if parts.len() == 3 || parts.len() == 4 {
                let red = parts[0].parse::<u8>().ok()?;
                let green = parts[1].parse::<u8>().ok()?;
                let blue = parts[2].parse::<u8>().ok()?;
                let alpha = if parts.len() == 4 {
                    let af = parts[3].parse::<f32>().ok()?.clamp(0.0, 1.0);
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    {
                        (af * 255.0).round() as u8
                    }
                } else {
                    255
                };
                return Some(Self::new(red, green, blue, alpha));
            }
        }

        match lower.as_str() {
            "black" => Some(Self::BLACK),
            "white" => Some(Self::WHITE),
            "transparent" | "none" => Some(Self::TRANSPARENT),
            "red" => Some(Self::rgb(255, 0, 0)),
            "green" => Some(Self::rgb(0, 128, 0)),
            "blue" => Some(Self::rgb(0, 0, 255)),
            "yellow" => Some(Self::rgb(255, 255, 0)),
            "cyan" | "aqua" => Some(Self::rgb(0, 255, 255)),
            "magenta" | "fuchsia" => Some(Self::rgb(255, 0, 255)),
            "gray" | "grey" => Some(Self::rgb(128, 128, 128)),
            "lightgray" | "lightgrey" | "silver" => Some(Self::rgb(192, 192, 192)),
            "darkgray" | "darkgrey" => Some(Self::rgb(64, 64, 64)),
            "orange" => Some(Self::rgb(255, 165, 0)),
            _ => {
                // Also accept bare 6-character hex strings (common in OOXML styles)
                if trimmed.len() == 6 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
                    let red = u8::from_str_radix(&trimmed[0..2], 16).ok()?;
                    let green = u8::from_str_radix(&trimmed[2..4], 16).ok()?;
                    let blue = u8::from_str_radix(&trimmed[4..6], 16).ok()?;
                    Some(Self::rgb(red, green, blue))
                } else {
                    None
                }
            }
        }
    }

    /// Format as a `#RRGGBB` hex string.
    #[must_use]
    pub fn to_hex_rgb(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }

    /// Normalized opacity (`0.0..=1.0`).
    #[must_use]
    pub fn opacity(self) -> f32 {
        f32::from(self.a) / 255.0
    }
}

/// Identifier of a font in [`FontStore`].
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct FontId(pub u32);

/// Identifier of an image in [`ImageStore`].
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct ImageId(pub u32);

/// Origin of a resolved font.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum FontSourceKind {
    /// Embedded directly in the document (e.g. DOCX `.odttf`, PPTX font, PDF font stream).
    Embedded,
    /// Bundled metric-compatible font shipped with Prism (`assets/fonts/`).
    Bundled,
    /// Host operating-system font (only when `LayoutOptions::allow_system_fonts` is enabled).
    System,
    /// Pre-extracted glyph outlines (e.g. PDF Type1/Type3 fonts without raw TrueType tables).
    SynthesizedOutlines,
}

/// Single entry in the per-document [`FontReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FontResolutionEntry {
    /// Font family name requested by the document.
    pub requested_family: String,
    /// Font family name actually used by the layout engine.
    pub resolved_family: String,
    /// Source category of the resolved font.
    pub source: FontSourceKind,
    /// Whether the resolved family matched the requested family without substitution.
    pub exact_match: bool,
}

/// Per-document font substitution and resolution report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FontReport {
    /// Map from requested family name to its resolution outcome, in deterministic sorted order.
    pub entries: BTreeMap<String, FontResolutionEntry>,
}

impl FontReport {
    /// Create an empty [`FontReport`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a font resolution mapping.
    pub fn record(
        &mut self,
        requested_family: impl Into<String>,
        resolved_family: impl Into<String>,
        source: FontSourceKind,
    ) {
        let req = requested_family.into();
        let res = resolved_family.into();
        let exact_match = req.eq_ignore_ascii_case(&res);
        self.entries.insert(
            req.clone(),
            FontResolutionEntry {
                requested_family: req,
                resolved_family: res,
                source,
                exact_match,
            },
        );
    }
}

/// A registered font face in [`FontStore`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FontEntry {
    /// Unique identifier in [`FontStore`].
    pub id: FontId,
    /// Resolved family name (e.g. `"Carlito"`).
    pub family: String,
    /// CSS font weight (`100..=900`, `400` = normal, `700` = bold).
    pub weight: u16,
    /// Whether this face is italic/oblique.
    pub italic: bool,
    /// Source of this font face.
    pub source: FontSourceKind,
    /// Raw TrueType/OpenType font bytes (empty only for synthesized outline fonts).
    pub data: Vec<u8>,
    /// Face index inside a font collection (`.ttc`), usually `0`.
    pub face_index: u32,
    /// Font units per em (`upem`), read from the `head` table or `1000`.
    pub units_per_em: u16,
    /// Normalized typographic ascender (`> 0`, relative to `1.0` em).
    pub ascent: f32,
    /// Normalized typographic descender (`> 0` downward magnitude, relative to `1.0` em).
    pub descent: f32,
    /// Optional precomputed glyph outlines normalized to [`NORMALIZED_UPEM`] (`1000.0` upem).
    pub precomputed_outlines: BTreeMap<u16, Vec<PathCommand>>,
}

/// Deterministic store of fonts used by a [`LayoutDocument`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FontStore {
    /// Fonts keyed by [`FontId`] in deterministic order.
    pub fonts: BTreeMap<FontId, FontEntry>,
}

impl FontStore {
    /// Create an empty [`FontStore`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a [`FontEntry`].
    pub fn insert(&mut self, entry: FontEntry) -> FontId {
        let id = entry.id;
        self.fonts.insert(id, entry);
        id
    }

    /// Register a TrueType/OpenType font blob and inspect its metrics via `skrifa`.
    pub fn register_ttf(
        &mut self,
        family: impl Into<String>,
        weight: u16,
        italic: bool,
        source: FontSourceKind,
        data: Vec<u8>,
        face_index: u32,
    ) -> FontId {
        #[allow(clippy::cast_possible_truncation)]
        let id = FontId(self.fonts.len() as u32);
        let (units_per_em, ascent, descent) =
            if let Ok(font_ref) = skrifa::FontRef::from_index(&data, face_index) {
                let metrics = font_ref.metrics(SkrifaSize::unscaled(), LocationRef::default());
                let upem = metrics.units_per_em.max(1);
                let upem_f = f32::from(upem);
                let asc = (metrics.ascent / upem_f).abs().max(0.75);
                let desc = (metrics.descent / upem_f).abs().max(0.2);
                (upem, asc, desc)
            } else {
                (1000, 0.8, 0.2)
            };

        self.fonts.insert(
            id,
            FontEntry {
                id,
                family: family.into(),
                weight,
                italic,
                source,
                data,
                face_index,
                units_per_em,
                ascent,
                descent,
                precomputed_outlines: BTreeMap::new(),
            },
        );
        id
    }

    /// Look up a [`FontEntry`] by [`FontId`].
    #[must_use]
    pub fn get(&self, id: FontId) -> Option<&FontEntry> {
        self.fonts.get(&id)
    }

    /// Cache a precomputed `1000.0`-upem outline for `glyph_id` in `font_id` (used for PDF Type1/CFF/subset fonts).
    pub fn insert_precomputed_outline(
        &mut self,
        font_id: FontId,
        glyph_id: u16,
        commands: Vec<PathCommand>,
    ) {
        if let Some(entry) = self.fonts.get_mut(&font_id) {
            entry
                .precomputed_outlines
                .entry(glyph_id)
                .or_insert(commands);
        }
    }

    /// Extract the unhinted outline of `glyph_id` in `font_id` normalized to [`NORMALIZED_UPEM`] (`1000.0` units per em),
    /// with `+y` pointing downward (matching [`LayoutPage`] coordinates).
    #[must_use]
    pub fn glyph_outline(&self, font_id: FontId, glyph_id: u16) -> Option<Vec<PathCommand>> {
        let entry = self.fonts.get(&font_id)?;
        if let Some(precomputed) = entry.precomputed_outlines.get(&glyph_id) {
            return Some(precomputed.clone());
        }
        if entry.data.is_empty() {
            return None;
        }
        let font_ref = skrifa::FontRef::from_index(&entry.data, entry.face_index).ok()?;
        let outlines = font_ref.outline_glyphs();
        let glyph = outlines.get(skrifa::GlyphId::from(glyph_id))?;
        let mut pen = NormalizedOutlinePen {
            commands: Vec::new(),
        };
        glyph
            .draw(
                DrawSettings::unhinted(SkrifaSize::new(NORMALIZED_UPEM), LocationRef::default()),
                &mut pen,
            )
            .ok()?;
        Some(pen.commands)
    }
}

struct NormalizedOutlinePen {
    commands: Vec<PathCommand>,
}

impl OutlinePen for NormalizedOutlinePen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.commands.push(PathCommand::MoveTo(Point::new(
            normalize_zero(x),
            normalize_zero(-y),
        )));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.commands.push(PathCommand::LineTo(Point::new(
            normalize_zero(x),
            normalize_zero(-y),
        )));
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.commands.push(PathCommand::QuadTo {
            ctrl: Point::new(normalize_zero(cx0), normalize_zero(-cy0)),
            to: Point::new(normalize_zero(x), normalize_zero(-y)),
        });
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.commands.push(PathCommand::CurveTo {
            ctrl1: Point::new(normalize_zero(cx0), normalize_zero(-cy0)),
            ctrl2: Point::new(normalize_zero(cx1), normalize_zero(-cy1)),
            to: Point::new(normalize_zero(x), normalize_zero(-y)),
        });
    }

    fn close(&mut self) {
        self.commands.push(PathCommand::Close);
    }
}

/// Normalize `-0.0` to `+0.0` for deterministic serialization and formatting.
#[must_use]
pub fn normalize_zero(v: f32) -> f32 {
    if v.abs() < 1e-7 { 0.0 } else { v }
}

/// A raster image resource stored in [`ImageStore`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageEntry {
    /// Unique identifier in [`ImageStore`].
    pub id: ImageId,
    /// MIME type (e.g. `"image/png"`, `"image/jpeg"`).
    pub mime_type: String,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Encoded image bytes (or raw RGBA bytes when `mime_type == "image/x-rgba8"`).
    pub data: Vec<u8>,
}

/// Deterministic store of raster images used by a [`LayoutDocument`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImageStore {
    /// Images keyed by [`ImageId`] in deterministic order.
    pub images: BTreeMap<ImageId, ImageEntry>,
}

impl ImageStore {
    /// Create an empty [`ImageStore`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an image after verifying its dimensions against `options.max_image_pixels`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MemoryLimitExceeded`] if `width * height > options.max_image_pixels`.
    pub fn register(
        &mut self,
        mime_type: impl Into<String>,
        width: u32,
        height: u32,
        data: Vec<u8>,
        options: &LayoutOptions,
    ) -> Result<ImageId> {
        let pixels = u64::from(width) * u64::from(height);
        if pixels > options.max_image_pixels {
            return Err(Error::MemoryLimitExceeded {
                used: usize::try_from(pixels).unwrap_or(usize::MAX),
                limit: usize::try_from(options.max_image_pixels).unwrap_or(usize::MAX),
            });
        }
        #[allow(clippy::cast_possible_truncation)]
        let id = ImageId(self.images.len() as u32);
        self.images.insert(
            id,
            ImageEntry {
                id,
                mime_type: mime_type.into(),
                width,
                height,
                data,
            },
        );
        Ok(id)
    }

    /// Look up an [`ImageEntry`] by [`ImageId`].
    #[must_use]
    pub fn get(&self, id: ImageId) -> Option<&ImageEntry> {
        self.images.get(&id)
    }
}

/// A positioned document produced by `prism-layout`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LayoutDocument {
    /// Positioned pages in document order.
    pub pages: Vec<LayoutPage>,
    /// Font store containing all faces referenced by [`GlyphRun`]s.
    pub fonts: FontStore,
    /// Image store containing all raster images referenced by [`ImageItem`]s.
    pub images: ImageStore,
    /// Per-document font substitution and resolution report.
    pub font_report: FontReport,
}

impl LayoutDocument {
    /// Create an empty [`LayoutDocument`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// A single positioned page in a [`LayoutDocument`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutPage {
    /// Zero-based page index (`0..page_count`).
    pub index: u32,
    /// Page dimensions in points.
    pub size: Size,
    /// Positioned drawing and metadata items in back-to-front Z-order.
    pub items: Vec<Item>,
    /// Optional human-readable page label (e.g. `"i"`, `"1"`).
    pub label: Option<String>,
}

impl LayoutPage {
    /// Create an empty [`LayoutPage`] with the given `index` and `size`.
    #[must_use]
    pub fn new(index: u32, size: Size) -> Self {
        Self {
            index,
            size,
            items: Vec::new(),
            label: None,
        }
    }
}

/// A positioned item on a [`LayoutPage`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Item {
    /// A positioned run of glyphs sharing a font, size, color, and transform.
    Glyphs(GlyphRun),
    /// A filled and/or stroked vector path.
    Path(PathItem),
    /// A positioned raster image.
    Image(ImageItem),
    /// A group of items with an affine transform, optional clip path, and opacity.
    Group(GroupItem),
    /// An inert hyperlink region (shown as metadata/tooltip only, never navigated).
    Link(LinkItem),
    /// A named anchor or target rectangle (for bookmarks, comments, and redactions).
    Anchor(AnchorItem),
}

/// A positioned run of shaped glyphs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlyphRun {
    /// Font identifier in [`FontStore`].
    pub font: FontId,
    /// Font size in points.
    pub size: f32,
    /// Fill color of the glyphs.
    pub color: Rgba,
    /// Transform mapping run space (where `(0, 0)` is the run baseline origin) to page/group space.
    pub transform: Affine,
    /// Positioned glyphs in visual order.
    pub glyphs: Vec<PositionedGlyph>,
    /// UTF-8 source text represented by this run.
    pub text: String,
    /// Grapheme clusters mapping UTF-8 byte ranges in [`Self::text`] to glyph ranges in [`Self::glyphs`].
    pub clusters: Vec<Cluster>,
    /// Text decorations (underline, strikethrough, background highlight).
    pub decorations: Decorations,
    /// Back-reference to the originating UDM page, content block, and run.
    pub source: SourceRef,
}

/// A single positioned glyph within a [`GlyphRun`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct PositionedGlyph {
    /// Glyph index within the font face.
    pub glyph_id: u16,
    /// Horizontal offset from the [`GlyphRun`] origin in points (along the baseline).
    pub x: f32,
    /// Vertical offset from the [`GlyphRun`] baseline in points (`+y` down).
    pub y: f32,
    /// Horizontal advance of this glyph in points.
    pub advance: f32,
}

/// Mapping between a UTF-8 byte range in [`GlyphRun::text`] and a glyph range in [`GlyphRun::glyphs`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cluster {
    /// UTF-8 byte range (`start..end`) within [`GlyphRun::text`].
    pub text_range: Range<u32>,
    /// Glyph index range (`start..end`) within [`GlyphRun::glyphs`].
    pub glyph_range: Range<u32>,
    /// Horizontal start offset of this cluster from the [`GlyphRun`] origin in points.
    pub x: f32,
    /// Horizontal advance of this cluster in points.
    pub advance: f32,
}

/// Underline, strikethrough, and highlight decorations on a [`GlyphRun`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Decorations {
    /// Optional underline decoration.
    pub underline: Option<LineDecoration>,
    /// Optional strikethrough decoration.
    pub strikethrough: Option<LineDecoration>,
    /// Optional background highlight color.
    pub highlight: Option<Rgba>,
}

/// Parameters for an underline or strikethrough line.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LineDecoration {
    /// Line color.
    pub color: Rgba,
    /// Vertical offset from the baseline in points (`+y` down; positive for underline, negative for strikethrough).
    pub offset: f32,
    /// Line thickness in points.
    pub thickness: f32,
}

/// Back-reference to the UDM source element that produced an item.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceRef {
    /// Zero-based UDM page index.
    pub page: u32,
    /// Zero-based content block index on the UDM page.
    pub block: u32,
    /// Zero-based run index within the content block.
    pub run: u32,
}

impl SourceRef {
    /// Construct a new [`SourceRef`].
    #[must_use]
    pub const fn new(page: u32, block: u32, run: u32) -> Self {
        Self { page, block, run }
    }
}

/// A vector path item on a [`LayoutPage`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PathItem {
    /// Path geometry commands.
    pub commands: Vec<PathCommand>,
    /// Fill paint, if filled.
    pub fill: Option<FillPaint>,
    /// Winding rule used for filling.
    pub fill_rule: FillRule,
    /// Stroke style, if stroked.
    pub stroke: Option<StrokeStyle>,
    /// Transform mapping path coordinates to page/group space.
    pub transform: Affine,
}

/// A 2D vector path command.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum PathCommand {
    /// Move current subpath point to `Point`.
    MoveTo(Point),
    /// Line from current point to `Point`.
    LineTo(Point),
    /// Quadratic Bézier curve to `to` with control point `ctrl`.
    QuadTo {
        /// Quadratic control point.
        ctrl: Point,
        /// End point.
        to: Point,
    },
    /// Cubic Bézier curve to `to` with control points `ctrl1` and `ctrl2`.
    CurveTo {
        /// First cubic control point.
        ctrl1: Point,
        /// Second cubic control point.
        ctrl2: Point,
        /// End point.
        to: Point,
    },
    /// Close current subpath.
    Close,
}

/// Fill winding rule.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FillRule {
    /// Non-zero winding rule.
    #[default]
    NonZero,
    /// Even-odd winding rule.
    EvenOdd,
}

/// Paint used to fill a [`PathItem`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FillPaint {
    /// Solid RGBA color.
    Solid(Rgba),
    /// Linear gradient paint.
    LinearGradient(LinearGradient),
    /// Radial gradient paint.
    RadialGradient(RadialGradient),
}

/// A linear gradient between `start` and `end` in path space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinearGradient {
    /// Start point in path coordinates.
    pub start: Point,
    /// End point in path coordinates.
    pub end: Point,
    /// Ordered color stops (`offset` in `0.0..=1.0`).
    pub stops: Vec<GradientStop>,
}

/// A radial gradient centered at `center` with `radius` in path space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RadialGradient {
    /// Center point in path coordinates.
    pub center: Point,
    /// Radius in points.
    pub radius: f32,
    /// Ordered color stops (`offset` in `0.0..=1.0`).
    pub stops: Vec<GradientStop>,
}

/// A single stop in a [`LinearGradient`] or [`RadialGradient`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GradientStop {
    /// Normalized stop offset (`0.0..=1.0`).
    pub offset: f32,
    /// Stop color.
    pub color: Rgba,
}

/// Stroke style for a [`PathItem`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrokeStyle {
    /// Stroke color.
    pub color: Rgba,
    /// Stroke width in points.
    pub width: f32,
    /// Line cap style.
    pub line_cap: LineCap,
    /// Line join style.
    pub line_join: LineJoin,
    /// Dash pattern lengths in points (empty for solid line).
    pub dash_array: Vec<f32>,
    /// Dash offset in points.
    pub dash_offset: f32,
}

impl Default for StrokeStyle {
    fn default() -> Self {
        Self {
            color: Rgba::BLACK,
            width: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            dash_array: Vec::new(),
            dash_offset: 0.0,
        }
    }
}

/// Stroke line cap style.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineCap {
    /// Flat cap at the endpoint.
    #[default]
    Butt,
    /// Semicircular cap.
    Round,
    /// Square cap extending half the stroke width past the endpoint.
    Square,
}

/// Stroke line join style.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineJoin {
    /// Sharp miter join.
    #[default]
    Miter,
    /// Rounded join.
    Round,
    /// Beveled join.
    Bevel,
}

/// A positioned raster image on a [`LayoutPage`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageItem {
    /// Identifier of the image in [`ImageStore`].
    pub image: ImageId,
    /// Destination rectangle in local coordinates before [`Self::transform`].
    pub rect: Rect,
    /// Transform mapping local coordinates to page/group space.
    pub transform: Affine,
    /// Opacity multiplier (`0.0..=1.0`).
    pub opacity: f32,
    /// Optional accessibility description.
    pub alt_text: Option<String>,
}

/// A grouped subtree of [`Item`]s sharing a transform, clip path, and opacity.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GroupItem {
    /// Transform applied to all children.
    pub transform: Affine,
    /// Optional clipping path in local group coordinates.
    pub clip: Option<ClipPath>,
    /// Group opacity (`0.0..=1.0`).
    pub opacity: f32,
    /// Child items in Z-order.
    pub children: Vec<Item>,
}

/// A clipping path applied to a [`GroupItem`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClipPath {
    /// Clipping path commands.
    pub commands: Vec<PathCommand>,
    /// Clipping winding rule.
    pub fill_rule: FillRule,
}

/// An inert hyperlink region on a [`LayoutPage`].
///
/// For forensic safety, links are never navigated; viewers surface `target` as inert metadata only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkItem {
    /// Bounding rectangle in page coordinates.
    pub rect: Rect,
    /// Original target URI or internal destination string.
    pub target: String,
}

/// A named position or region on a [`LayoutPage`] (for bookmarks, comments, or redactions).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnchorItem {
    /// Anchor identifier.
    pub name: String,
    /// Anchor bounds in page coordinates.
    pub rect: Rect,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rgba_parsing() {
        assert_eq!(Rgba::from_css_str("#ff8000"), Some(Rgba::rgb(255, 128, 0)));
        assert_eq!(Rgba::from_css_str("#f0a"), Some(Rgba::rgb(255, 0, 170)));
        assert_eq!(
            Rgba::from_css_str("rgba(10, 20, 30, 0.5)"),
            Some(Rgba::new(10, 20, 30, 128))
        );
        assert_eq!(
            Rgba::from_css_str("4472C4"),
            Some(Rgba::rgb(0x44, 0x72, 0xC4))
        );
        assert_eq!(Rgba::from_css_str("white"), Some(Rgba::WHITE));
    }

    #[test]
    fn test_affine_transforms() {
        let t = Affine::translate(10.0, 20.0).then(Affine::scale(2.0));
        let p = t.transform_point(Point::new(5.0, 7.0));
        assert!((p.x - 20.0).abs() < 1e-4);
        assert!((p.y - 34.0).abs() < 1e-4);
    }

    #[test]
    fn test_image_store_pixel_cap() {
        let mut store = ImageStore::new();
        let opts = LayoutOptions {
            max_image_pixels: 1000,
            ..LayoutOptions::default()
        };

        assert!(
            store
                .register("image/png", 20, 20, vec![0; 4], &opts)
                .is_ok()
        );
        assert!(
            store
                .register("image/png", 100, 100, vec![0; 4], &opts)
                .is_err()
        );
    }
}

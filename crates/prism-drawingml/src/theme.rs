// SPDX-License-Identifier: AGPL-3.0-only
//! `DrawingML` theme (`a:theme`) and color transform pipeline (`lumMod`, `lumOff`, `tint`, `shade`, `satMod`, `alpha`).

use prism_layout::model::Rgba;
use quick_xml::Reader;
use quick_xml::events::Event;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Color modification operation in `DrawingML` (ECMA-376 §20.1.2.3).
///
/// Operations are percentages in `0.0..=1.0` (where `100_000` in XML = `1.0`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ColorTransform {
    /// Multiply luminance (`a:lumMod`).
    LumMod(f32),
    /// Offset luminance (`a:lumOff`).
    LumOff(f32),
    /// Tint toward white in sRGB/linear space (`a:tint`).
    Tint(f32),
    /// Shade toward black (`a:shade`).
    Shade(f32),
    /// Multiply saturation (`a:satMod`).
    SatMod(f32),
    /// Offset saturation (`a:satOff`).
    SatOff(f32),
    /// Multiply hue (`a:hueMod`).
    HueMod(f32),
    /// Offset hue in degrees `0..360` (`a:hueOff`).
    HueOff(f32),
    /// Set alpha (`a:alpha`).
    Alpha(f32),
    /// Multiply alpha (`a:alphaMod`).
    AlphaMod(f32),
    /// Offset alpha (`a:alphaOff`).
    AlphaOff(f32),
    /// Complement color (`a:comp`).
    Comp,
    /// Invert RGB channels (`a:inv`).
    Inv,
}

/// Parsed Office `DrawingML` theme (`theme1.xml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrawingTheme {
    /// Theme name (`a:theme/@name`).
    pub name: String,
    /// Scheme colors (`dk1`, `lt1`, `dk2`, `lt2`, `accent1`..`accent6`, `hlink`, `folHlink`).
    pub colors: BTreeMap<String, Rgba>,
    /// Major Latin font family (`a:majorFont/a:latin`).
    pub major_latin_font: String,
    /// Minor Latin font family (`a:minorFont/a:latin`).
    pub minor_latin_font: String,
    /// Major East Asian font family.
    pub major_ea_font: Option<String>,
    /// Minor East Asian font family.
    pub minor_ea_font: Option<String>,
    /// Major Complex Script font family.
    pub major_cs_font: Option<String>,
    /// Minor Complex Script font family.
    pub minor_cs_font: Option<String>,
}

impl Default for DrawingTheme {
    fn default() -> Self {
        let mut colors = BTreeMap::new();
        // Standard Office theme defaults
        colors.insert("dk1".to_string(), Rgba::rgb(0x00, 0x00, 0x00));
        colors.insert("lt1".to_string(), Rgba::rgb(0xFF, 0xFF, 0xFF));
        colors.insert("dk2".to_string(), Rgba::rgb(0x1F, 0x49, 0x7D));
        colors.insert("lt2".to_string(), Rgba::rgb(0xEE, 0xEC, 0xE1));
        colors.insert("accent1".to_string(), Rgba::rgb(0x4F, 0x81, 0xBD));
        colors.insert("accent2".to_string(), Rgba::rgb(0xC0, 0x50, 0x4D));
        colors.insert("accent3".to_string(), Rgba::rgb(0x9B, 0xBB, 0x59));
        colors.insert("accent4".to_string(), Rgba::rgb(0x80, 0x64, 0xA2));
        colors.insert("accent5".to_string(), Rgba::rgb(0x4B, 0xAC, 0xC6));
        colors.insert("accent6".to_string(), Rgba::rgb(0xF7, 0x96, 0x46));
        colors.insert("hlink".to_string(), Rgba::rgb(0x00, 0x00, 0xFF));
        colors.insert("folHlink".to_string(), Rgba::rgb(0x80, 0x00, 0x80));

        Self {
            name: "Office Theme".to_string(),
            colors,
            major_latin_font: "Calibri Light".to_string(),
            minor_latin_font: "Calibri".to_string(),
            major_ea_font: None,
            minor_ea_font: None,
            major_cs_font: None,
            minor_cs_font: None,
        }
    }
}

impl DrawingTheme {
    /// Resolve a scheme color name (`tx1`, `bg1`, `tx2`, `bg2`, `phClr`, `dk1`..`folHlink`) to [`Rgba`].
    #[must_use]
    pub fn resolve_scheme_color(&self, scheme_name: &str) -> Option<Rgba> {
        let canonical = match scheme_name {
            "tx1" => "dk1",
            "bg1" => "lt1",
            "tx2" => "dk2",
            "bg2" => "lt2",
            "phClr" => "accent1",
            other => other,
        };
        self.colors.get(canonical).copied()
    }

    /// Resolve a scheme color and apply a slice of [`ColorTransform`]s in order.
    #[must_use]
    pub fn resolve_transformed(
        &self,
        scheme_name: &str,
        transforms: &[ColorTransform],
    ) -> Option<Rgba> {
        let base = self.resolve_scheme_color(scheme_name)?;
        Some(apply_color_transforms(base, transforms))
    }

    /// Return the 6 accent colors in order (`accent1`..=`accent6`) for chart series palettes.
    #[must_use]
    pub fn accent_palette(&self) -> [Rgba; 6] {
        [
            self.resolve_scheme_color("accent1")
                .unwrap_or(Rgba::rgb(0x4F, 0x81, 0xBD)),
            self.resolve_scheme_color("accent2")
                .unwrap_or(Rgba::rgb(0xC0, 0x50, 0x4D)),
            self.resolve_scheme_color("accent3")
                .unwrap_or(Rgba::rgb(0x9B, 0xBB, 0x59)),
            self.resolve_scheme_color("accent4")
                .unwrap_or(Rgba::rgb(0x80, 0x64, 0xA2)),
            self.resolve_scheme_color("accent5")
                .unwrap_or(Rgba::rgb(0x4B, 0xAC, 0xC6)),
            self.resolve_scheme_color("accent6")
                .unwrap_or(Rgba::rgb(0xF7, 0x96, 0x46)),
        ]
    }
}

/// Parse a 6-digit or 8-digit hex string (with or without leading `#`) into [`Rgba`].
#[must_use]
pub fn parse_hex_rgba(hex: &str) -> Option<Rgba> {
    let clean = hex.trim().trim_start_matches('#');
    // Byte slicing below needs ASCII (a non-ASCII value would split a character).
    if !clean.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    if clean.len() == 6 {
        let r = u8::from_str_radix(&clean[0..2], 16).ok()?;
        let g = u8::from_str_radix(&clean[2..4], 16).ok()?;
        let b = u8::from_str_radix(&clean[4..6], 16).ok()?;
        Some(Rgba::rgb(r, g, b))
    } else if clean.len() == 8 {
        let r = u8::from_str_radix(&clean[0..2], 16).ok()?;
        let g = u8::from_str_radix(&clean[2..4], 16).ok()?;
        let b = u8::from_str_radix(&clean[4..6], 16).ok()?;
        let a = u8::from_str_radix(&clean[6..8], 16).ok()?;
        Some(Rgba::new(r, g, b, a))
    } else {
        None
    }
}

/// Apply `DrawingML` [`ColorTransform`]s in sequence per ECMA-376 §20.1.2.3.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]
pub fn apply_color_transforms(base: Rgba, transforms: &[ColorTransform]) -> Rgba {
    if transforms.is_empty() {
        return base;
    }

    let mut r = f32::from(base.r) / 255.0;
    let mut g = f32::from(base.g) / 255.0;
    let mut b = f32::from(base.b) / 255.0;
    let mut a = f32::from(base.a) / 255.0;

    for &t in transforms {
        match t {
            ColorTransform::LumMod(m) => {
                let (h, s, mut l) = rgb_to_hsl(r, g, b);
                l = (l * m).clamp(0.0, 1.0);
                (r, g, b) = hsl_to_rgb(h, s, l);
            }
            ColorTransform::LumOff(off) => {
                let (h, s, mut l) = rgb_to_hsl(r, g, b);
                l = (l + off).clamp(0.0, 1.0);
                (r, g, b) = hsl_to_rgb(h, s, l);
            }
            ColorTransform::SatMod(m) => {
                let (h, mut s, l) = rgb_to_hsl(r, g, b);
                s = (s * m).clamp(0.0, 1.0);
                (r, g, b) = hsl_to_rgb(h, s, l);
            }
            ColorTransform::SatOff(off) => {
                let (h, mut s, l) = rgb_to_hsl(r, g, b);
                s = (s + off).clamp(0.0, 1.0);
                (r, g, b) = hsl_to_rgb(h, s, l);
            }
            ColorTransform::HueMod(m) => {
                let (mut h, s, l) = rgb_to_hsl(r, g, b);
                h = (h * m).rem_euclid(360.0);
                (r, g, b) = hsl_to_rgb(h, s, l);
            }
            ColorTransform::HueOff(off) => {
                let (mut h, s, l) = rgb_to_hsl(r, g, b);
                h = (h + off).rem_euclid(360.0);
                (r, g, b) = hsl_to_rgb(h, s, l);
            }
            ColorTransform::Tint(tint) => {
                let k = tint.clamp(0.0, 1.0);
                r = (1.0 - (1.0 - r) * k).clamp(0.0, 1.0);
                g = (1.0 - (1.0 - g) * k).clamp(0.0, 1.0);
                b = (1.0 - (1.0 - b) * k).clamp(0.0, 1.0);
            }
            ColorTransform::Shade(shade) => {
                let k = shade.clamp(0.0, 1.0);
                r = (r * k).clamp(0.0, 1.0);
                g = (g * k).clamp(0.0, 1.0);
                b = (b * k).clamp(0.0, 1.0);
            }
            ColorTransform::Alpha(alpha) => {
                a = alpha.clamp(0.0, 1.0);
            }
            ColorTransform::AlphaMod(m) => {
                a = (a * m).clamp(0.0, 1.0);
            }
            ColorTransform::AlphaOff(off) => {
                a = (a + off).clamp(0.0, 1.0);
            }
            ColorTransform::Comp => {
                let (mut h, s, l) = rgb_to_hsl(r, g, b);
                h = (h + 180.0).rem_euclid(360.0);
                (r, g, b) = hsl_to_rgb(h, s, l);
            }
            ColorTransform::Inv => {
                r = 1.0 - r;
                g = 1.0 - g;
                b = 1.0 - b;
            }
        }
    }

    Rgba::new(
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
        (a * 255.0).round() as u8,
    )
}

#[allow(clippy::many_single_char_names)]
fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = f32::midpoint(max, min);

    if (max - min).abs() <= f32::EPSILON {
        return (0.0, 0.0, l);
    }

    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };

    let h = if (max - r).abs() <= f32::EPSILON {
        ((g - b) / d + if g < b { 6.0 } else { 0.0 }) * 60.0
    } else if (max - g).abs() <= f32::EPSILON {
        ((b - r) / d + 2.0) * 60.0
    } else {
        ((r - g) / d + 4.0) * 60.0
    };

    (h, s, l)
}

#[allow(clippy::many_single_char_names)]
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    if s <= f32::EPSILON {
        return (l, l, l);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let hn = h.rem_euclid(360.0) / 360.0;
    (
        hue_to_rgb(p, q, hn + 1.0 / 3.0),
        hue_to_rgb(p, q, hn),
        hue_to_rgb(p, q, hn - 1.0 / 3.0),
    )
}

fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 0.5 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    p
}

/// Parse an Office `theme1.xml` string into a [`DrawingTheme`].
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn parse_drawing_theme(xml: &str) -> DrawingTheme {
    let mut theme = DrawingTheme::default();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut in_clr_scheme = false;
    let mut current_slot: Option<String> = None;
    let mut in_major_font = false;
    let mut in_minor_font = false;

    loop {
        match reader
            .read_event_into(&mut buf)
            .map(crate::xml::resolve_general_ref)
        {
            Ok(Event::Start(ref e) | Event::Empty(ref e)) => {
                let qname = e.name();
                let local = local_name(qname.as_ref());
                match local {
                    b"theme" => {
                        if let Some(n) = attr_val(e, b"name") {
                            theme.name = n;
                        }
                    }
                    b"clrScheme" => in_clr_scheme = true,
                    b"dk1" | b"lt1" | b"dk2" | b"lt2" | b"accent1" | b"accent2" | b"accent3"
                    | b"accent4" | b"accent5" | b"accent6" | b"hlink" | b"folHlink"
                        if in_clr_scheme =>
                    {
                        current_slot = Some(String::from_utf8_lossy(local).into_owned());
                    }
                    b"srgbClr" if current_slot.is_some() => {
                        if let Some(val) = attr_val(e, b"val")
                            && let Some(rgba) = parse_hex_rgba(&val)
                            && let Some(ref slot) = current_slot
                        {
                            theme.colors.insert(slot.clone(), rgba);
                        }
                    }
                    b"sysClr" if current_slot.is_some() => {
                        let hex = attr_val(e, b"lastClr").or_else(|| attr_val(e, b"val"));
                        if let Some(h) = hex
                            && let Some(rgba) = parse_hex_rgba(&h)
                            && let Some(ref slot) = current_slot
                        {
                            theme.colors.insert(slot.clone(), rgba);
                        }
                    }
                    b"majorFont" => in_major_font = true,
                    b"minorFont" => in_minor_font = true,
                    b"latin" if in_major_font || in_minor_font => {
                        if let Some(tf) = attr_val(e, b"typeface")
                            && !tf.is_empty()
                        {
                            if in_major_font {
                                theme.major_latin_font = tf;
                            } else {
                                theme.minor_latin_font = tf;
                            }
                        }
                    }
                    b"ea" if in_major_font || in_minor_font => {
                        if let Some(tf) = attr_val(e, b"typeface")
                            && !tf.is_empty()
                        {
                            if in_major_font {
                                theme.major_ea_font = Some(tf);
                            } else {
                                theme.minor_ea_font = Some(tf);
                            }
                        }
                    }
                    b"cs" if in_major_font || in_minor_font => {
                        if let Some(tf) = attr_val(e, b"typeface")
                            && !tf.is_empty()
                        {
                            if in_major_font {
                                theme.major_cs_font = Some(tf);
                            } else {
                                theme.minor_cs_font = Some(tf);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                let qname = e.name();
                match local_name(qname.as_ref()) {
                    b"clrScheme" => {
                        in_clr_scheme = false;
                        current_slot = None;
                    }
                    b"dk1" | b"lt1" | b"dk2" | b"lt2" | b"accent1" | b"accent2" | b"accent3"
                    | b"accent4" | b"accent5" | b"accent6" | b"hlink" | b"folHlink" => {
                        current_slot = None;
                    }
                    b"majorFont" => in_major_font = false,
                    b"minorFont" => in_minor_font = false,
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    theme
}

pub(crate) fn local_name(raw: &str) -> &[u8] {
    let bytes = raw.as_bytes();
    bytes
        .iter()
        .rposition(|&b| b == b':')
        .map_or(bytes, |idx| &bytes[idx + 1..])
}

pub(crate) fn attr_val(e: &quick_xml::events::BytesStart<'_>, key: &[u8]) -> Option<String> {
    for attr in e.attributes().flatten() {
        let k = attr.key;
        if local_name(k.as_ref()) == key {
            let raw = attr.value.as_ref();
            return Some(
                quick_xml::escape::unescape(raw)
                    .map_or_else(|_| raw.to_owned(), std::borrow::Cow::into_owned),
            );
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_theme_and_applies_lum_mod_lum_off_tint_shade() {
        let xml = r#"<?xml version="1.0"?>
        <a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Custom">
          <a:themeElements>
            <a:clrScheme name="Custom">
              <a:dk1><a:sysClr val="windowText" lastClr="112233"/></a:dk1>
              <a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1>
              <a:accent1><a:srgbClr val="4472C4"/></a:accent1>
            </a:clrScheme>
            <a:fontScheme name="Custom">
              <a:majorFont><a:latin typeface="Aptos Display"/></a:majorFont>
              <a:minorFont><a:latin typeface="Aptos"/></a:minorFont>
            </a:fontScheme>
          </a:themeElements>
        </a:theme>"#;

        let theme = parse_drawing_theme(xml);
        assert_eq!(theme.name, "Custom");
        assert_eq!(theme.major_latin_font, "Aptos Display");
        assert_eq!(theme.minor_latin_font, "Aptos");
        assert_eq!(
            theme.resolve_scheme_color("tx1"),
            Some(Rgba::rgb(0x11, 0x22, 0x33))
        );

        let lighter = theme
            .resolve_transformed(
                "accent1",
                &[ColorTransform::LumMod(0.6), ColorTransform::LumOff(0.4)],
            )
            .unwrap();
        assert!(lighter.r > 0x44 && lighter.g > 0x72 && lighter.b > 0xC4);
    }

    #[test]
    fn non_ascii_hex_colors_are_rejected() {
        assert_eq!(parse_hex_rgba("12€456"), None);
        assert_eq!(parse_hex_rgba("1234€6"), None);
        assert_eq!(parse_hex_rgba("FF0000"), Some(Rgba::rgb(255, 0, 0)));
    }
}

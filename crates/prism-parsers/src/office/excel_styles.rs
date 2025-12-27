// SPDX-License-Identifier: AGPL-3.0-only
//! Excel styles parser
//!
//! Parses styles.xml to extract fonts, fills, borders, and cell formatting (XFs).

use crate::office::theme::Theme;
use crate::office::utils;
use prism_core::error::{Error, Result};
use quick_xml::events::Event;
use quick_xml::Reader;

/// Represents a font configuration in Excel
#[derive(Debug, Clone, Default)]
pub struct ExcelFont {
    pub name: String,
    pub size: f64,
    pub color: Option<String>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// Represents a fill configuration (background color)
#[derive(Debug, Clone, Default)]
pub struct ExcelFill {
    pub pattern_type: String,
    pub fg_color: Option<String>,
    pub bg_color: Option<String>,
}

/// Represents a cell format (XF) which links to font, fill, and border
#[derive(Debug, Clone, Default)]
pub struct CellXf {
    pub font_id: usize,
    pub fill_id: usize,
    pub border_id: usize,
    pub num_fmt_id: usize,
    pub align_h: Option<String>,
    pub align_v: Option<String>,
}

/// Collection of all styles in the workbook
#[derive(Debug, Clone, Default)]
pub struct ExcelStyles {
    pub fonts: Vec<ExcelFont>,
    pub fills: Vec<ExcelFill>,
    pub cell_xfs: Vec<CellXf>,
}

fn map_theme_index(idx: i64) -> Option<&'static str> {
    match idx {
        0 => Some("lt1"),
        1 => Some("dk1"),
        2 => Some("lt2"),
        3 => Some("dk2"),
        4 => Some("accent1"),
        5 => Some("accent2"),
        6 => Some("accent3"),
        7 => Some("accent4"),
        8 => Some("accent5"),
        9 => Some("accent6"),
        10 => Some("hlink"),
        11 => Some("folHlink"),
        _ => None,
    }
}

impl ExcelStyles {
    pub fn from_xml(xml: &str, theme: Option<&Theme>) -> Result<Self> {
        let mut reader = Reader::from_str(xml);
        reader.trim_text(true);

        let mut fonts = Vec::new();
        let mut fills = Vec::new();
        let mut cell_xfs = Vec::new();

        let mut buf = Vec::new();

        // State tracking
        let mut in_fonts = false;
        let mut in_fills = false;
        let mut in_cell_xfs = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => match e.name().as_ref() {
                    b"fonts" => in_fonts = true,
                    b"fills" => in_fills = true,
                    b"cellXfs" => in_cell_xfs = true,
                    b"font" if in_fonts => {
                        let font = parse_font(&mut reader, theme)?;
                        fonts.push(font);
                    }
                    b"fill" if in_fills => {
                        let fill = parse_fill(&mut reader, theme)?;
                        fills.push(fill);
                    }
                    b"xf" if in_cell_xfs => {
                        let mut xf = CellXf::default();
                        for attr in e.attributes().flatten() {
                            match attr.key.as_ref() {
                                b"fontId" => {
                                    xf.font_id =
                                        utils::attr_value(&attr.value).parse().unwrap_or(0);
                                }
                                b"fillId" => {
                                    xf.fill_id =
                                        utils::attr_value(&attr.value).parse().unwrap_or(0);
                                }
                                b"borderId" => {
                                    xf.border_id =
                                        utils::attr_value(&attr.value).parse().unwrap_or(0);
                                }
                                b"numFmtId" => {
                                    xf.num_fmt_id =
                                        utils::attr_value(&attr.value).parse().unwrap_or(0);
                                }
                                _ => {}
                            }
                        }
                        let mut xf_buf = Vec::new();
                        loop {
                            match reader.read_event_into(&mut xf_buf) {
                                Ok(Event::Empty(ref sub_e)) => {
                                    if sub_e.name().as_ref() == b"alignment" {
                                        if let Some(h) = utils::attr_value_opt(sub_e, b"horizontal")
                                        {
                                            xf.align_h = Some(h);
                                        }
                                        if let Some(v) = utils::attr_value_opt(sub_e, b"vertical") {
                                            xf.align_v = Some(v);
                                        }
                                    }
                                }
                                Ok(Event::End(ref sub_e)) if sub_e.name().as_ref() == b"xf" => {
                                    break;
                                }
                                Ok(Event::Eof) => break,
                                _ => {}
                            }
                            xf_buf.clear();
                        }
                        cell_xfs.push(xf);
                    }
                    _ => {}
                },
                Ok(Event::Empty(e)) => {
                    if e.name().as_ref() == b"xf" && in_cell_xfs {
                        let mut xf = CellXf::default();
                        for attr in e.attributes().flatten() {
                            match attr.key.as_ref() {
                                b"fontId" => {
                                    xf.font_id =
                                        utils::attr_value(&attr.value).parse().unwrap_or(0);
                                }
                                b"fillId" => {
                                    xf.fill_id =
                                        utils::attr_value(&attr.value).parse().unwrap_or(0);
                                }
                                b"borderId" => {
                                    xf.border_id =
                                        utils::attr_value(&attr.value).parse().unwrap_or(0);
                                }
                                b"numFmtId" => {
                                    xf.num_fmt_id =
                                        utils::attr_value(&attr.value).parse().unwrap_or(0);
                                }
                                _ => {}
                            }
                        }
                        cell_xfs.push(xf);
                    }
                }
                Ok(Event::End(e)) => match e.name().as_ref() {
                    b"fonts" => in_fonts = false,
                    b"fills" => in_fills = false,
                    b"cellXfs" => in_cell_xfs = false,
                    _ => {}
                },
                Ok(Event::Eof) => break,
                Err(e) => return Err(Error::ParseError(format!("XML error in styles: {e}"))),
                _ => {}
            }
            buf.clear();
        }

        Ok(Self {
            fonts,
            fills,
            cell_xfs,
        })
    }

    /// Resolve a style by XF index/ID
    pub fn get_style(&self, xf_id: usize) -> (prism_core::document::TextStyle, Option<String>) {
        use prism_core::document::TextStyle;

        if xf_id >= self.cell_xfs.len() {
            return (TextStyle::default(), None);
        }

        let xf = &self.cell_xfs[xf_id];

        // Font
        let mut text_style = TextStyle::default();
        if xf.font_id < self.fonts.len() {
            let font = &self.fonts[xf.font_id];
            text_style.font_family = Some(font.name.clone());
            text_style.font_size = Some(font.size);
            text_style.bold = font.bold;
            text_style.italic = font.italic;
            text_style.underline = font.underline;

            if let Some(ref c) = font.color {
                text_style.color = Some(c.clone());
            }
        }

        // Fill (Background)
        let mut bg_color = None;
        if xf.fill_id < self.fills.len() {
            let fill = &self.fills[xf.fill_id];
            // Prefer fg_color for solid fills in Excel XML structure
            if let Some(ref c) = fill.fg_color {
                bg_color = Some(c.clone());
            } else if let Some(ref c) = fill.bg_color {
                bg_color = Some(c.clone());
            }
        }

        (text_style, bg_color)
    }
}

/// Helper to parse a <font> element
fn parse_font(reader: &mut Reader<&[u8]>, theme: Option<&Theme>) -> Result<ExcelFont> {
    let mut font = ExcelFont::default();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref sub_e)) => {
                match sub_e.name().as_ref() {
                    b"sz" => {
                        if let Some(val) = utils::attr_value_opt(sub_e, b"val") {
                            if let Ok(s) = val.parse::<f64>() {
                                font.size = s;
                            }
                        }
                    }
                    b"name" => {
                        if let Some(val) = utils::attr_value_opt(sub_e, b"val") {
                            font.name = val;
                        }
                    }
                    b"color" => {
                        let mut rgb: Option<String> = None;
                        let mut theme_idx: Option<i64> = None;
                        let mut tint: Option<f64> = None;

                        for attr in sub_e.attributes().flatten() {
                            match attr.key.as_ref() {
                                b"rgb" => rgb = Some(utils::attr_value(&attr.value)),
                                b"theme" => {
                                    if let Ok(v) = utils::attr_value(&attr.value).parse::<i64>() {
                                        theme_idx = Some(v);
                                    }
                                }
                                b"tint" => {
                                    if let Ok(v) = utils::attr_value(&attr.value).parse::<f64>() {
                                        tint = Some(v);
                                    }
                                }
                                _ => {}
                            }
                        }

                        // Resolve
                        let mut final_color = None;
                        if let Some(r) = rgb {
                            final_color = Some(format!("#{r}"));
                        } else if let Some(idx) = theme_idx {
                            if let Some(t) = theme {
                                if let Some(ref_str) = map_theme_index(idx) {
                                    if let Some(c) = utils::resolve_word_color(
                                        None,
                                        Some(ref_str),
                                        tint,
                                        None,
                                        t,
                                    ) {
                                        final_color = Some(c);
                                    }
                                }
                            }
                        }
                        font.color = final_color;
                    }
                    b"b" => font.bold = true,
                    b"i" => font.italic = true,
                    b"u" => font.underline = true,
                    _ => {}
                }
            }
            Ok(Event::End(ref sub_e)) if sub_e.name().as_ref() == b"font" => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(Error::ParseError(format!("XML error in font: {e}"))),
            _ => {}
        }
        buf.clear();
    }
    Ok(font)
}

/// Helper to parse a <fill> element
fn parse_fill(reader: &mut Reader<&[u8]>, theme: Option<&Theme>) -> Result<ExcelFill> {
    let mut fill = ExcelFill::default();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref sub_e)) => {
                if sub_e.name().as_ref() == b"patternFill" {
                    if let Some(pt) = utils::attr_value_opt(sub_e, b"patternType") {
                        fill.pattern_type = pt;
                    }
                }
            }
            Ok(Event::Empty(ref sub_e)) => {
                if sub_e.name().as_ref() == b"patternFill" {
                    if let Some(pt) = utils::attr_value_opt(sub_e, b"patternType") {
                        fill.pattern_type = pt;
                    }
                } else if sub_e.name().as_ref() == b"fgColor" || sub_e.name().as_ref() == b"bgColor"
                {
                    let is_fg = sub_e.name().as_ref() == b"fgColor";
                    let mut rgb: Option<String> = None;
                    let mut theme_idx: Option<i64> = None;
                    let mut tint: Option<f64> = None;

                    for attr in sub_e.attributes().flatten() {
                        match attr.key.as_ref() {
                            b"rgb" => rgb = Some(utils::attr_value(&attr.value)),
                            b"theme" => {
                                if let Ok(v) = utils::attr_value(&attr.value).parse::<i64>() {
                                    theme_idx = Some(v);
                                }
                            }
                            b"tint" => {
                                if let Ok(v) = utils::attr_value(&attr.value).parse::<f64>() {
                                    tint = Some(v);
                                }
                            }
                            _ => {}
                        }
                    }

                    // Resolve
                    let mut final_color = None;
                    if let Some(r) = rgb {
                        final_color = Some(format!("#{r}"));
                    } else if let Some(idx) = theme_idx {
                        if let Some(t) = theme {
                            if let Some(ref_str) = map_theme_index(idx) {
                                if let Some(c) =
                                    utils::resolve_word_color(None, Some(ref_str), tint, None, t)
                                {
                                    final_color = Some(c);
                                }
                            }
                        }
                    }

                    if is_fg {
                        fill.fg_color = final_color;
                    } else {
                        fill.bg_color = final_color;
                    }
                }
            }
            Ok(Event::End(ref sub_e)) if sub_e.name().as_ref() == b"fill" => break,
            Ok(Event::Eof) => break,
            Err(e) => return Err(Error::ParseError(format!("XML error in fill: {e}"))),
            _ => {}
        }
        buf.clear();
    }
    Ok(fill)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_excel_theme_color_resolution() {
        let theme_xml = r#"
            <a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Office Theme">
                <a:themeElements>
                    <a:clrScheme name="Office">
                        <a:dk1>
                            <a:sysClr val="windowText" lastClr="000000"/>
                        </a:dk1>
                        <a:lt1>
                            <a:sysClr val="window" lastClr="FFFFFF"/>
                        </a:lt1>
                        <a:dk2>
                            <a:srgbClr val="44546A"/>
                        </a:dk2>
                        <a:lt2>
                            <a:srgbClr val="E7E6E6"/>
                        </a:lt2>
                        <a:accent1>
                            <a:srgbClr val="4472C4"/>
                        </a:accent1>
                        <a:accent2>
                            <a:srgbClr val="ED7D31"/>
                        </a:accent2>
                    </a:clrScheme>
                </a:themeElements>
            </a:theme>
        "#;
        let theme = crate::office::theme::parse_theme(theme_xml.as_bytes()).unwrap();

        let styles_xml = r#"
            <styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
                <fonts>
                    <font>
                        <name val="Calibri"/>
                        <color theme="4"/> <!-- accent1 -> 4472C4 -->
                    </font>
                    <font>
                        <name val="Arial"/>
                        <color theme="5" tint="-0.25"/> <!-- accent2 -> ED7D31 darkened -->
                    </font>
                </fonts>
                <fills>
                    <fill>
                        <patternFill patternType="solid">
                            <fgColor theme="0"/> <!-- lt1 -> FFFFFF (using lastClr) -->
                        </patternFill>
                    </fill>
                </fills>
                <cellXfs>
                    <xf fontId="0" fillId="0" borderId="0" numFmtId="0"/>
                    <xf fontId="1" fillId="0" borderId="0" numFmtId="0"/>
                </cellXfs>
            </styleSheet>
        "#;

        let styles = ExcelStyles::from_xml(styles_xml, Some(&theme)).unwrap();

        // Check Font 0 (Accent 1)
        let font0 = &styles.fonts[0];
        assert_eq!(font0.color.as_deref(), Some("#4472C4"));

        // Check Font 1 (Accent 2 with tint)
        let font1 = &styles.fonts[1];
        assert!(font1.color.is_some());
        let c = font1.color.as_ref().unwrap();
        assert_ne!(c, "#ED7D31"); // Should be modified

        // Check Fill (lt1) - window -> FFFFFF
        let fill0 = &styles.fills[0];
        assert_eq!(fill0.fg_color.as_deref(), Some("#FFFFFF"));
    }
}

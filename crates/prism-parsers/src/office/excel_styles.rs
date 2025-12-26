// SPDX-License-Identifier: AGPL-3.0-only
//! Excel styles parser
//!
//! Parses styles.xml to extract fonts, fills, borders, and cell formatting (XFs).

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

impl ExcelStyles {
    pub fn from_xml(xml: &str) -> Result<Self> {
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
                Ok(Event::Start(e)) => {
                    match e.name().as_ref() {
                        b"fonts" => in_fonts = true,
                        b"fills" => in_fills = true,
                        b"cellXfs" => in_cell_xfs = true,
                        b"font" if in_fonts => {
                            // Parse font
                            let mut font = ExcelFont::default();
                            // Inner elements like <sz val="11"/>, <name val="Calibri"/> are usually separate child tags in Office XML
                            // But quick-xml streaming means we need to handle children here.
                            // Actually, standard quick-xml loop structure is flat.
                            // We need to either use a sub-loop or track "current object being built".
                            // For simplicity/robustness, let's use a sub-loop for the complex objects.

                            // NOTE: Current structure of quick-xml loop implies we are at Start("font").
                            // We will process until End("font").
                            let mut font_buf = Vec::new();
                            loop {
                                match reader.read_event_into(&mut font_buf) {
                                    Ok(Event::Empty(ref sub_e)) => {
                                        match sub_e.name().as_ref() {
                                            b"sz" => {
                                                if let Some(val) =
                                                    utils::attr_value_opt(sub_e, b"val")
                                                {
                                                    if let Ok(s) = val.parse::<f64>() {
                                                        font.size = s;
                                                    }
                                                }
                                            }
                                            b"name" => {
                                                if let Some(val) =
                                                    utils::attr_value_opt(sub_e, b"val")
                                                {
                                                    font.name = val;
                                                }
                                            }
                                            b"color" => {
                                                if let Some(rgb) =
                                                    utils::attr_value_opt(sub_e, b"rgb")
                                                {
                                                    font.color = Some(format!("#{}", rgb));
                                                }
                                                // Theme color support would be here (b"theme")
                                            }
                                            b"b" => font.bold = true,
                                            b"i" => font.italic = true,
                                            b"u" => font.underline = true,
                                            _ => {}
                                        }
                                    }
                                    Ok(Event::End(ref sub_e))
                                        if sub_e.name().as_ref() == b"font" =>
                                    {
                                        break
                                    }
                                    Ok(Event::Eof) => break,
                                    _ => {}
                                }
                                font_buf.clear();
                            }
                            fonts.push(font);
                        }
                        b"fill" if in_fills => {
                            // Pattern fill is usually nested inside <fill><patternFill>...</patternFill></fill>
                            let mut fill = ExcelFill::default();
                            let mut fill_buf = Vec::new();
                            loop {
                                match reader.read_event_into(&mut fill_buf) {
                                    Ok(Event::Start(ref sub_e)) => {
                                        if sub_e.name().as_ref() == b"patternFill" {
                                            if let Some(pt) =
                                                utils::attr_value_opt(sub_e, b"patternType")
                                            {
                                                fill.pattern_type = pt;
                                            }
                                        }
                                        // Start events for colors would mean they have children, which is rare for colors in Excel XML (usually empty tags)
                                    }
                                    Ok(Event::Empty(ref sub_e)) => {
                                        if sub_e.name().as_ref() == b"patternFill" {
                                            if let Some(pt) =
                                                utils::attr_value_opt(sub_e, b"patternType")
                                            {
                                                fill.pattern_type = pt;
                                            }
                                        } else if sub_e.name().as_ref() == b"fgColor" {
                                            if let Some(rgb) = utils::attr_value_opt(sub_e, b"rgb")
                                            {
                                                fill.fg_color = Some(format!("#{}", rgb));
                                            }
                                        } else if sub_e.name().as_ref() == b"bgColor" {
                                            if let Some(rgb) = utils::attr_value_opt(sub_e, b"rgb")
                                            {
                                                fill.bg_color = Some(format!("#{}", rgb));
                                            }
                                        }
                                    }
                                    Ok(Event::End(ref sub_e))
                                        if sub_e.name().as_ref() == b"fill" =>
                                    {
                                        break
                                    }
                                    Ok(Event::Eof) => break,
                                    _ => {}
                                }
                                fill_buf.clear();
                            }
                            fills.push(fill);
                        }
                        b"xf" if in_cell_xfs => {
                            let mut xf = CellXf::default();
                            // attributes on the tag itself
                            for attr in e.attributes().flatten() {
                                match attr.key.as_ref() {
                                    b"fontId" => {
                                        xf.font_id =
                                            utils::attr_value(&attr.value).parse().unwrap_or(0)
                                    }
                                    b"fillId" => {
                                        xf.fill_id =
                                            utils::attr_value(&attr.value).parse().unwrap_or(0)
                                    }
                                    b"borderId" => {
                                        xf.border_id =
                                            utils::attr_value(&attr.value).parse().unwrap_or(0)
                                    }
                                    b"numFmtId" => {
                                        xf.num_fmt_id =
                                            utils::attr_value(&attr.value).parse().unwrap_or(0)
                                    }
                                    _ => {}
                                }
                            }
                            // Check children for alignment
                            let mut xf_buf = Vec::new();
                            loop {
                                // XF can be self-closing <xf ... /> or have children <alignment ... />
                                // If self closing, we are already Done?
                                // quick-xml Empty event means self-closing. Start event means it has children.
                                // Wait, we are in a match arm for Start(e). If it was Empty, we would be in Empty arm.
                                // So we need to handle Empty(e) for xf in the main loop if we want to support self-closing xfs.
                                // Let's modify the main loop structure below or handle it here if valid valid XML structure warrants it.
                                // Assuming Start event here, look for End(xf)
                                match reader.read_event_into(&mut xf_buf) {
                                    Ok(Event::Empty(ref sub_e)) => {
                                        if sub_e.name().as_ref() == b"alignment" {
                                            if let Some(h) =
                                                utils::attr_value_opt(sub_e, b"horizontal")
                                            {
                                                xf.align_h = Some(h);
                                            }
                                            if let Some(v) =
                                                utils::attr_value_opt(sub_e, b"vertical")
                                            {
                                                xf.align_v = Some(v);
                                            }
                                        }
                                    }
                                    Ok(Event::End(ref sub_e)) if sub_e.name().as_ref() == b"xf" => {
                                        break
                                    }
                                    Ok(Event::Eof) => break,
                                    _ => {}
                                }
                                xf_buf.clear();
                            }
                            cell_xfs.push(xf);
                        }
                        _ => {}
                    }
                }
                Ok(Event::Empty(e)) => {
                    // Handle self-closing xf tags
                    if e.name().as_ref() == b"xf" && in_cell_xfs {
                        let mut xf = CellXf::default();
                        for attr in e.attributes().flatten() {
                            match attr.key.as_ref() {
                                b"fontId" => {
                                    xf.font_id = utils::attr_value(&attr.value).parse().unwrap_or(0)
                                }
                                b"fillId" => {
                                    xf.fill_id = utils::attr_value(&attr.value).parse().unwrap_or(0)
                                }
                                b"borderId" => {
                                    xf.border_id =
                                        utils::attr_value(&attr.value).parse().unwrap_or(0)
                                }
                                b"numFmtId" => {
                                    xf.num_fmt_id =
                                        utils::attr_value(&attr.value).parse().unwrap_or(0)
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
                Err(e) => return Err(Error::ParseError(format!("XML error in styles: {}", e))),
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
}

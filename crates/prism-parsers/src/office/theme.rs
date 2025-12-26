// SPDX-License-Identifier: AGPL-3.0-only
use crate::office::utils::attr_value;
use prism_core::error::{Error, Result};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Theme {
    pub name: String,
    pub color_scheme: HashMap<String, String>,
    pub major_font: Option<String>,
    pub minor_font: Option<String>,
}

impl Theme {
    /// Resolve a color reference (e.g., "accent1") to a hex string
    pub fn resolve_color(&self, color_ref: &str) -> Option<String> {
        self.color_scheme.get(color_ref).cloned()
    }
}

pub fn parse_theme(content: &[u8]) -> Result<Theme> {
    let mut reader = Reader::from_reader(content);
    reader.trim_text(true);

    let mut theme = Theme::default();
    let mut buf = Vec::new();
    let mut in_clr_scheme = false;
    let mut in_font_scheme = false;
    let mut in_major_font = false;
    let mut in_minor_font = false;

    // Track current color tag specifically to extract val="..."
    // e.g. <a:dk1><a:sysClr val="..."/></a:dk1>
    let mut current_clr_tag: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let name = e.name();
                match name.as_ref() {
                    b"a:theme" => {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"name" {
                                theme.name = attr_value(&attr.value);
                            }
                        }
                    }
                    b"a:clrScheme" => {
                        in_clr_scheme = true;
                    }
                    b"a:fontScheme" => {
                        in_font_scheme = true;
                    }
                    b"a:majorFont" => {
                        if in_font_scheme {
                            in_major_font = true;
                        }
                    }
                    b"a:minorFont" => {
                        if in_font_scheme {
                            in_minor_font = true;
                        }
                    }
                    b"a:latin" => {
                        if in_major_font || in_minor_font {
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"typeface" {
                                    let typeface = attr_value(&attr.value);
                                    if in_major_font {
                                        theme.major_font = Some(typeface);
                                    } else if in_minor_font {
                                        theme.minor_font = Some(typeface);
                                    }
                                    break;
                                }
                            }
                        }
                    }
                    _ => {
                        if in_clr_scheme {
                            let tag_name = String::from_utf8_lossy(name.as_ref()).to_string();

                            // Check for color definitions inside a color slot
                            if let Some(ref slot) = current_clr_tag {
                                if tag_name == "a:srgbClr" || tag_name == "a:sysClr" {
                                    for attr in e.attributes().flatten() {
                                        if attr.key.as_ref() == b"val" {
                                            let val = attr_value(&attr.value);
                                            // Extract actual name from "a:dk1" -> "dk1"
                                            let slot_key = slot.replace("a:", "");
                                            theme.color_scheme.insert(slot_key, val);
                                        }
                                        if attr.key.as_ref() == b"lastClr"
                                            && !theme
                                                .color_scheme
                                                .contains_key(&slot.replace("a:", ""))
                                        {
                                            let val = attr_value(&attr.value);
                                            let slot_key = slot.replace("a:", "");
                                            theme.color_scheme.insert(slot_key, val);
                                        }
                                    }
                                }
                            } else if tag_name.starts_with("a:") {
                                // Assume this is a color slot like a:dk1, a:lt1, a:accent1
                                current_clr_tag = Some(tag_name);
                            }
                        }
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let name = e.name();
                match name.as_ref() {
                    b"a:clrScheme" => in_clr_scheme = false,
                    b"a:fontScheme" => in_font_scheme = false,
                    b"a:majorFont" => in_major_font = false,
                    b"a:minorFont" => in_minor_font = false,
                    _ => {
                        if in_clr_scheme {
                            let tag_name = String::from_utf8_lossy(name.as_ref()).to_string();
                            if let Some(ref current) = current_clr_tag {
                                if *current == tag_name {
                                    current_clr_tag = None;
                                }
                            }
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(Error::ParseError(format!("XML error: {:?}", e))),
            _ => (),
        }
        buf.clear();
    }

    Ok(theme)
}

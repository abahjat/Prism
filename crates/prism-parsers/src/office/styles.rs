// SPDX-License-Identifier: AGPL-3.0-only
use crate::office::utils;
use prism_core::document::{ParagraphStyle, TextAlignment, TextStyle};
use prism_core::error::{Error, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Style {
    pub id: String,
    pub name: Option<String>,
    pub style_type: String, // paragraph, character, numbering, table
    pub based_on: Option<String>,
    pub next: Option<String>,
    pub text_style: TextStyle,
    pub para_style: ParagraphStyle,
}

#[derive(Debug, Clone, Default)]
pub struct Styles {
    styles: HashMap<String, Style>,
    default_paragraph_style: ParagraphStyle,
    default_text_style: TextStyle,
}

impl Styles {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve effective text style for a paragraph/run
    /// TODO: Implement full inheritance (Style -> BasedOn -> Defaults)
    pub fn resolve_text_style(
        &self,
        style_id: Option<&str>,
        direct_formatting: &TextStyle,
    ) -> TextStyle {
        // Start with defaults (TODO)
        let mut resolved = self.default_text_style.clone();

        // Apply named style if present
        if let Some(id) = style_id {
            if let Some(style) = self.styles.get(id) {
                // Merge style properties
                if style.text_style.bold {
                    resolved.bold = true;
                }
                if style.text_style.italic {
                    resolved.italic = true;
                }
                if let Some(ref color) = style.text_style.color {
                    resolved.color = Some(color.clone());
                }
                if let Some(sz) = style.text_style.font_size {
                    resolved.font_size = Some(sz);
                }
                if let Some(ref font) = style.text_style.font_family {
                    resolved.font_family = Some(font.clone());
                }
            }
        }

        // Apply direct formatting (highest priority)
        if direct_formatting.bold {
            resolved.bold = true;
        }
        if direct_formatting.italic {
            resolved.italic = true;
        }
        if direct_formatting.underline {
            resolved.underline = true;
        }
        if let Some(ref color) = direct_formatting.color {
            resolved.color = Some(color.clone());
        }
        if let Some(sz) = direct_formatting.font_size {
            resolved.font_size = Some(sz);
        }
        if let Some(ref font) = direct_formatting.font_family {
            resolved.font_family = Some(font.clone());
        }

        resolved
    }

    pub fn from_xml(xml: &str) -> Result<Self> {
        let mut styles = HashMap::new();
        let mut reader = Reader::from_str(xml);
        reader.trim_text(true);
        let mut buf = Vec::new();

        let mut current_style: Option<Style> = None;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    let name = e.name();
                    if name.as_ref() == b"w:style" {
                        let mut id = String::new();
                        let mut style_type = String::new();

                        for attr in e.attributes().flatten() {
                            match attr.key.as_ref() {
                                b"w:styleId" => id = utils::attr_value(&attr.value),
                                b"w:type" => style_type = utils::attr_value(&attr.value),
                                _ => {}
                            }
                        }

                        current_style = Some(Style {
                            id,
                            name: None,
                            style_type,
                            based_on: None,
                            next: None,
                            text_style: TextStyle::default(),
                            para_style: ParagraphStyle::default(),
                        });
                    } else if let Some(style) = &mut current_style {
                        match name.as_ref() {
                            b"w:name" => {
                                for attr in e.attributes().flatten() {
                                    if attr.key.as_ref() == b"w:val" {
                                        style.name = Some(utils::attr_value(&attr.value));
                                    }
                                }
                            }
                            b"w:basedOn" => {
                                for attr in e.attributes().flatten() {
                                    if attr.key.as_ref() == b"w:val" {
                                        style.based_on = Some(utils::attr_value(&attr.value));
                                    }
                                }
                            }
                            b"w:b" => style.text_style.bold = true,
                            b"w:i" => style.text_style.italic = true,
                            b"w:u" => style.text_style.underline = true,
                            b"w:color" => {
                                for attr in e.attributes().flatten() {
                                    if attr.key.as_ref() == b"w:val" {
                                        let val = utils::attr_value(&attr.value);
                                        if val != "auto" {
                                            style.text_style.color = Some(format!("#{}", val));
                                        }
                                    }
                                }
                            }
                            b"w:sz" => {
                                for attr in e.attributes().flatten() {
                                    if attr.key.as_ref() == b"w:val" {
                                        if let Ok(val) =
                                            utils::attr_value(&attr.value).parse::<f64>()
                                        {
                                            style.text_style.font_size = Some(val / 2.0);
                                        }
                                    }
                                }
                            }
                            b"w:jc" => {
                                for attr in e.attributes().flatten() {
                                    if attr.key.as_ref() == b"w:val" {
                                        let val = utils::attr_value(&attr.value);
                                        style.para_style.alignment = match val.as_str() {
                                            "center" => TextAlignment::Center,
                                            "right" => TextAlignment::Right,
                                            "both" => TextAlignment::Justify,
                                            _ => TextAlignment::Left,
                                        };
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Ok(Event::Empty(e)) => {
                    // Handle empty tags like <w:b/> inside a style
                    if let Some(style) = &mut current_style {
                        match e.name().as_ref() {
                            b"w:b" => style.text_style.bold = true,
                            b"w:i" => style.text_style.italic = true,
                            b"w:u" => style.text_style.underline = true,
                            // TODO: Handle more empty tags
                            _ => {}
                        }
                    }
                }
                Ok(Event::End(e)) => {
                    if e.name().as_ref() == b"w:style" {
                        if let Some(style) = current_style.take() {
                            styles.insert(style.id.clone(), style);
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(Error::ParseError(format!("XML error in styles: {}", e))),
                _ => {}
            }
            buf.clear();
        }

        Ok(Self {
            styles,
            default_paragraph_style: ParagraphStyle::default(),
            default_text_style: TextStyle::default(),
        })
    }
}

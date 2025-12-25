use crate::office::utils;
use prism_core::document::{ContentBlock, Rect, TextBlock, TextRun, TextStyle};
use quick_xml::events::Event;
use quick_xml::Reader;

/// Parse a shape element (p:sp) into a ContentBlock
pub fn parse_shape(reader: &mut Reader<&[u8]>, buf: &mut Vec<u8>) -> Option<ContentBlock> {
    let mut bounds = Rect::default();
    let mut text_runs = Vec::new();
    let mut in_tx_body = false;
    let mut current_paragraph_text = String::new();

    // Helper to finish a paragraph
    let finish_paragraph = |runs: &mut Vec<TextRun>, text: &mut String| {
        if !text.is_empty() {
            runs.push(TextRun {
                text: text.clone(),
                style: TextStyle::default(), // TODO: Parse run properties (a:rPr)
                bounds: None,
                char_positions: None,
            });
            text.clear();
        }
    };

    loop {
        match reader.read_event_into(buf) {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"a:off" => {
                    for attr in e.attributes().flatten() {
                        match attr.key.as_ref() {
                            b"x" => {
                                if let Ok(val) = utils::attr_value(&attr.value).parse::<f64>() {
                                    bounds.x = val / 12700.0; // EMU to points (approx 12700 EMUs per point)
                                }
                            }
                            b"y" => {
                                if let Ok(val) = utils::attr_value(&attr.value).parse::<f64>() {
                                    bounds.y = val / 12700.0;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                b"a:ext" => {
                    for attr in e.attributes().flatten() {
                        match attr.key.as_ref() {
                            b"cx" => {
                                if let Ok(val) = utils::attr_value(&attr.value).parse::<f64>() {
                                    bounds.width = val / 12700.0;
                                }
                            }
                            b"cy" => {
                                if let Ok(val) = utils::attr_value(&attr.value).parse::<f64>() {
                                    bounds.height = val / 12700.0;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                b"p:txBody" => in_tx_body = true,
                b"a:p" => {
                    // Start of paragraph
                }
                b"a:t" => {
                    if in_tx_body {
                        if let Ok(text) = reader.read_text(e.name()) {
                            current_paragraph_text.push_str(&text);
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"p:sp" => break,
                b"p:txBody" => in_tx_body = false,
                b"a:p" => {
                    finish_paragraph(&mut text_runs, &mut current_paragraph_text);
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }

    if text_runs.is_empty() {
        return None;
    }

    Some(ContentBlock::Text(TextBlock {
        bounds,
        runs: text_runs,
        paragraph_style: None,
    }))
}

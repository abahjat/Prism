// SPDX-License-Identifier: AGPL-3.0-only
//! # Slides Module
//!
//! Functionality for parsing PowerPoint slides.

use crate::office::shapes;
use prism_core::document::{ContentBlock, Dimensions, Page, PageMetadata};
use quick_xml::events::Event;
use quick_xml::Reader;

pub struct SlideParser;

impl SlideParser {
    pub fn parse(
        xml: &str,
        slide_num: u32,
        rels: &std::collections::HashMap<String, String>,
        dimensions: Dimensions,
    ) -> Page {
        let mut reader = Reader::from_str(xml);
        reader.trim_text(true);
        let mut buf = Vec::new();
        let mut content = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => match e.name().as_ref() {
                    b"p:bg" => {
                        if let Some(mut block) =
                            shapes::parse_background(&mut reader, &mut Vec::new(), rels, dimensions)
                        {
                            if let ContentBlock::Image(ref mut img) = block {
                                if let Some(target) = rels.get(&img.resource_id) {
                                    img.resource_id = target.clone();
                                }
                            }
                            content.insert(0, block);
                        }
                    }
                    b"p:sp" => {
                        if let Some(block) = shapes::parse_shape(&mut reader, &mut Vec::new()) {
                            content.push(block);
                        }
                    }
                    b"p:pic" => {
                        if let Some(mut block) =
                            shapes::parse_picture(&mut reader, &mut Vec::new(), rels)
                        {
                            if let ContentBlock::Image(ref mut img) = block {
                                if let Some(target) = rels.get(&img.resource_id) {
                                    img.resource_id = target.clone();
                                }
                            }
                            content.push(block);
                        }
                    }
                    b"p:graphicFrame" => {
                        if let Some(block) =
                            shapes::parse_graphic_frame(&mut reader, &mut Vec::new())
                        {
                            content.push(block);
                        }
                    }
                    _ => {}
                },
                Ok(Event::Eof) => break,
                _ => {}
            }
            buf.clear();
        }

        Page {
            number: slide_num,
            dimensions,
            content,
            annotations: Vec::new(),
            metadata: PageMetadata {
                label: Some(format!("Slide {}", slide_num)),
                rotation: 0,
            },
        }
    }
}

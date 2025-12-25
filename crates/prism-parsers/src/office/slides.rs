use crate::office::shapes;
use prism_core::document::{Dimensions, Page, PageMetadata};
use quick_xml::events::Event;
use quick_xml::Reader;

pub struct SlideParser;

impl SlideParser {
    pub fn parse(xml: &str, slide_num: u32) -> Page {
        let mut reader = Reader::from_str(xml);
        reader.trim_text(true);
        let mut buf = Vec::new();
        let mut content = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    match e.name().as_ref() {
                        b"p:sp" => {
                            if let Some(block) = shapes::parse_shape(&mut reader, &mut Vec::new()) {
                                content.push(block);
                            }
                        }
                        // TODO: Handle p:pic (pictures) and p:graphicFrame (tables)
                        _ => {}
                    }
                }
                Ok(Event::Eof) => break,
                _ => {}
            }
            buf.clear();
        }

        Page {
            number: slide_num,
            dimensions: Dimensions::new(960.0, 540.0), // Default 16:9, TODO: Parse from presentation.xml
            content,
            annotations: Vec::new(),
            metadata: PageMetadata {
                label: Some(format!("Slide {}", slide_num)),
                rotation: 0,
            },
        }
    }
}

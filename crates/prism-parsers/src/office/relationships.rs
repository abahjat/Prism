use crate::office::utils;
use prism_core::error::{Error, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;

/// A relationship to an external or internal resource
#[derive(Debug, Clone)]
pub struct Relationship {
    pub id: String,
    pub target: String,
    pub rel_type: String,
}

/// Store for document relationships
#[derive(Debug, Clone, Default)]
pub struct Relationships {
    map: HashMap<String, Relationship>,
}

impl Relationships {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse relationships from XML content
    pub fn from_xml(xml: &str) -> Result<Self> {
        let mut map = HashMap::new();
        let mut reader = Reader::from_str(xml);
        reader.trim_text(true);

        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Empty(e)) | Ok(Event::Start(e)) => {
                    if e.name().as_ref() == b"Relationship" {
                        let mut id = None;
                        let mut target = None;
                        let mut rel_type = None;

                        for attr in e.attributes().flatten() {
                            match attr.key.as_ref() {
                                b"Id" => id = Some(utils::attr_value(&attr.value)),
                                b"Target" => target = Some(utils::attr_value(&attr.value)),
                                b"Type" => rel_type = Some(utils::attr_value(&attr.value)),
                                _ => {}
                            }
                        }

                        if let (Some(id), Some(target), Some(rel_type)) = (id, target, rel_type) {
                            map.insert(
                                id.clone(),
                                Relationship {
                                    id,
                                    target,
                                    rel_type,
                                },
                            );
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => {
                    return Err(Error::ParseError(format!(
                        "XML error in relationships: {}",
                        e
                    )))
                }
                _ => {}
            }
            buf.clear();
        }

        Ok(Self { map })
    }

    /// Get a relationship by ID
    pub fn get(&self, id: &str) -> Option<&Relationship> {
        self.map.get(id)
    }

    /// Find relationships by type
    pub fn find_by_type<'a>(&'a self, rel_type: &'a str) -> impl Iterator<Item = &'a Relationship> {
        self.map.values().filter(move |r| r.rel_type == rel_type)
    }
}

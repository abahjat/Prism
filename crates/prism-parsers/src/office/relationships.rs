// SPDX-License-Identifier: AGPL-3.0-only
//! # Relationships Module
//!
//! Handles parsing and management of Office Open XML relationships.

use crate::office::utils;
use prism_core::error::{Error, Result};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::collections::HashMap;

/// A relationship to an external or internal resource
#[derive(Debug, Clone)]
pub struct Relationship {
    /// Unique identifier for the relationship
    pub id: String,
    /// Target URI or path
    pub target: String,
    /// Relationship type URI
    pub rel_type: String,
}

/// Store for document relationships
#[derive(Debug, Clone, Default)]
pub struct Relationships {
    /// Map of relationship ID to Relationship object
    pub map: HashMap<String, Relationship>,
}

impl Relationships {
    /// Create a new empty relationships store
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse relationships from XML content
    ///
    /// # Errors
    /// Returns an error if the XML is malformed or cannot be parsed.
    pub fn from_xml(xml: &str) -> Result<Self> {
        let mut map = HashMap::new();
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);

        loop {
            match reader.read_event() {
                Ok(Event::Empty(e) | Event::Start(e)) => {
                    if e.name().as_ref() == "Relationship" {
                        let mut id = None;
                        let mut target = None;
                        let mut rel_type = None;

                        for attr in e.attributes().flatten() {
                            match attr.key.as_ref() {
                                "Id" => id = Some(utils::attr_value(&attr.value)),
                                "Target" => target = Some(utils::attr_value(&attr.value)),
                                "Type" => rel_type = Some(utils::attr_value(&attr.value)),
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
                        "XML error in relationships: {e}"
                    )));
                }
                _ => {}
            }
        }

        Ok(Self { map })
    }

    /// Get a relationship by ID
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Relationship> {
        self.map.get(id)
    }

    /// Find relationships by type
    pub fn find_by_type<'a>(
        &'a self,
        rel_type: &'a str,
    ) -> impl Iterator<Item = &'a Relationship> + 'a {
        self.map.values().filter(move |r| r.rel_type == rel_type)
    }
}

//! Parser registry for managing and discovering format parsers.

use prism_core::format::Format;
use prism_core::parser::Parser;
use std::collections::HashMap;
use std::sync::Arc;

/// Registry for managing format parsers
///
/// The registry maintains a collection of available parsers and provides
/// methods to find the appropriate parser for a given format.
#[derive(Clone, Default)]
pub struct ParserRegistry {
    parsers: HashMap<String, Arc<dyn Parser>>,
}

impl ParserRegistry {
    /// Create a new empty parser registry
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a parser for a specific format
    ///
    /// # Arguments
    ///
    /// * `parser` - The parser implementation to register
    pub fn register(&mut self, parser: Arc<dyn Parser>) {
        let format = parser.format();
        self.parsers.insert(format.mime_type.clone(), parser);
    }

    /// Get a parser for the given format
    ///
    /// # Arguments
    ///
    /// * `format` - The document format
    ///
    /// # Returns
    ///
    /// The registered parser for this format, if available
    #[must_use]
    pub fn get_parser(&self, format: &Format) -> Option<Arc<dyn Parser>> {
        self.parsers.get(&format.mime_type).cloned()
    }

    /// Get a parser for the given format and data
    ///
    /// This method checks if the parser can actually handle the specific file
    /// by calling can_parse() before returning it.
    ///
    /// # Arguments
    ///
    /// * `format` - The document format
    /// * `data` - The file data to verify the parser can handle
    ///
    /// # Returns
    ///
    /// The registered parser for this format if it can parse the data
    #[must_use]
    pub fn get_parser_for_data(&self, format: &Format, data: &[u8]) -> Option<Arc<dyn Parser>> {
        self.parsers.get(&format.mime_type).and_then(|parser| {
            if parser.can_parse(data) {
                Some(parser.clone())
            } else {
                None
            }
        })
    }

    /// Get all registered parsers
    #[must_use]
    pub fn all_parsers(&self) -> Vec<Arc<dyn Parser>> {
        self.parsers.values().cloned().collect()
    }

    /// Check if a parser is registered for the given format
    #[must_use]
    pub fn has_parser(&self, format: &Format) -> bool {
        self.parsers.contains_key(&format.mime_type)
    }

    /// Get the number of registered parsers
    #[must_use]
    pub fn count(&self) -> usize {
        self.parsers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_creation() {
        let registry = ParserRegistry::new();
        assert_eq!(registry.count(), 0);
    }

    #[test]
    fn test_has_parser() {
        let registry = ParserRegistry::new();
        let format = Format::pdf();
        assert!(!registry.has_parser(&format));
    }
}

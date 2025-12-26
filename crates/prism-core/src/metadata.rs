// SPDX-License-Identifier: AGPL-3.0-only
//! # Document Metadata
//!
//! Metadata structures for documents.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Document metadata
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Metadata {
    /// Document title
    pub title: Option<String>,

    /// Document author(s)
    pub author: Option<String>,

    /// Document subject/description
    pub subject: Option<String>,

    /// Keywords/tags
    pub keywords: Vec<String>,

    /// Creator application (e.g., "Microsoft Word")
    pub creator: Option<String>,

    /// Producer (e.g., PDF converter)
    pub producer: Option<String>,

    /// Creation date
    pub created: Option<DateTime<Utc>>,

    /// Last modified date
    pub modified: Option<DateTime<Utc>>,

    /// Language code (e.g., "en-US")
    pub language: Option<String>,

    /// Custom metadata properties
    pub custom: HashMap<String, MetadataValue>,
}

impl Metadata {
    /// Create a new empty metadata instance
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a builder for fluent construction
    #[must_use]
    pub fn builder() -> MetadataBuilder {
        MetadataBuilder::new()
    }

    /// Add a custom metadata property
    pub fn add_custom(&mut self, key: impl Into<String>, value: impl Into<MetadataValue>) {
        self.custom.insert(key.into(), value.into());
    }

    /// Get a custom metadata property
    #[must_use]
    pub fn get_custom(&self, key: &str) -> Option<&MetadataValue> {
        self.custom.get(key)
    }
}

/// Builder for constructing metadata
#[derive(Debug, Default)]
pub struct MetadataBuilder {
    metadata: Metadata,
}

impl MetadataBuilder {
    /// Create a new metadata builder
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the title
    #[must_use]
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.metadata.title = Some(title.into());
        self
    }

    /// Set the author
    #[must_use]
    pub fn author(mut self, author: impl Into<String>) -> Self {
        self.metadata.author = Some(author.into());
        self
    }

    /// Set the subject
    #[must_use]
    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.metadata.subject = Some(subject.into());
        self
    }

    /// Add a keyword
    #[must_use]
    pub fn keyword(mut self, keyword: impl Into<String>) -> Self {
        self.metadata.keywords.push(keyword.into());
        self
    }

    /// Set the creator
    #[must_use]
    pub fn creator(mut self, creator: impl Into<String>) -> Self {
        self.metadata.creator = Some(creator.into());
        self
    }

    /// Set the producer
    #[must_use]
    pub fn producer(mut self, producer: impl Into<String>) -> Self {
        self.metadata.producer = Some(producer.into());
        self
    }

    /// Set the creation date
    #[must_use]
    pub fn created(mut self, created: DateTime<Utc>) -> Self {
        self.metadata.created = Some(created);
        self
    }

    /// Set the modified date
    #[must_use]
    pub fn modified(mut self, modified: DateTime<Utc>) -> Self {
        self.metadata.modified = Some(modified);
        self
    }

    /// Set the language
    #[must_use]
    pub fn language(mut self, language: impl Into<String>) -> Self {
        self.metadata.language = Some(language.into());
        self
    }

    /// Add a custom property
    #[must_use]
    pub fn custom(mut self, key: impl Into<String>, value: impl Into<MetadataValue>) -> Self {
        self.metadata.custom.insert(key.into(), value.into());
        self
    }

    /// Build the final metadata
    #[must_use]
    pub fn build(self) -> Metadata {
        self.metadata
    }
}

/// A metadata value (can be string, number, bool, or date)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MetadataValue {
    /// String value
    String(String),

    /// Integer value
    Integer(i64),

    /// Floating point value
    Float(f64),

    /// Boolean value
    Boolean(bool),

    /// Date/time value
    DateTime(DateTime<Utc>),
}

impl From<String> for MetadataValue {
    fn from(s: String) -> Self {
        MetadataValue::String(s)
    }
}

impl From<&str> for MetadataValue {
    fn from(s: &str) -> Self {
        MetadataValue::String(s.to_string())
    }
}

impl From<i64> for MetadataValue {
    fn from(i: i64) -> Self {
        MetadataValue::Integer(i)
    }
}

impl From<f64> for MetadataValue {
    fn from(f: f64) -> Self {
        MetadataValue::Float(f)
    }
}

impl From<bool> for MetadataValue {
    fn from(b: bool) -> Self {
        MetadataValue::Boolean(b)
    }
}

impl From<DateTime<Utc>> for MetadataValue {
    fn from(dt: DateTime<Utc>) -> Self {
        MetadataValue::DateTime(dt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata_builder() {
        let metadata = Metadata::builder()
            .title("Test Document")
            .author("Test Author")
            .keyword("test")
            .keyword("example")
            .build();

        assert_eq!(metadata.title, Some("Test Document".to_string()));
        assert_eq!(metadata.author, Some("Test Author".to_string()));
        assert_eq!(metadata.keywords.len(), 2);
    }

    #[test]
    fn test_custom_metadata() {
        let mut metadata = Metadata::new();
        metadata.add_custom("custom_field", "custom_value");
        metadata.add_custom("number_field", 42_i64);

        assert!(metadata.get_custom("custom_field").is_some());
        assert!(metadata.get_custom("number_field").is_some());
    }

    #[test]
    fn test_metadata_value_conversions() {
        let _string_val: MetadataValue = "test".into();
        let _int_val: MetadataValue = 42_i64.into();
        let _float_val: MetadataValue = 3.14.into();
        let _bool_val: MetadataValue = true.into();
    }
}

//! # Error Handling
//!
//! Error types and result aliases for Prism operations.

use std::io;
use thiserror::Error;

/// Result type alias for Prism operations
pub type Result<T> = std::result::Result<T, Error>;

/// Main error type for Prism operations
#[derive(Error, Debug)]
pub enum Error {
    /// Format detection failed
    #[error("Unable to detect document format: {0}")]
    DetectionFailed(String),

    /// Unsupported format
    #[error("Unsupported document format: {0}")]
    UnsupportedFormat(String),

    /// Parser error
    #[error("Failed to parse document: {0}")]
    ParseError(String),

    /// Rendering error
    #[error("Failed to render document: {0}")]
    RenderError(String),

    /// Invalid input
    #[error("Invalid input: {0}")]
    InvalidInput(String),

    /// Document is encrypted/password-protected
    #[error("Document is encrypted: {0}")]
    Encrypted(String),

    /// Document is corrupted
    #[error("Document is corrupted: {0}")]
    Corrupted(String),

    /// Resource not found
    #[error("Resource not found: {0}")]
    ResourceNotFound(String),

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    /// Timeout
    #[error("Operation timed out after {0:?}")]
    Timeout(std::time::Duration),

    /// Memory limit exceeded
    #[error("Memory limit exceeded: {used} bytes used, {limit} bytes allowed")]
    MemoryLimitExceeded {
        /// Bytes used
        used: usize,
        /// Bytes allowed
        limit: usize,
    },

    /// Sandbox error
    #[error("Sandbox error: {0}")]
    SandboxError(String),

    /// Configuration error
    #[error("Configuration error: {0}")]
    ConfigError(String),

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

impl Error {
    /// Check if this error is recoverable
    #[must_use]
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            Error::Timeout(_) | Error::MemoryLimitExceeded { .. }
        )
    }

    /// Check if this error is due to invalid/corrupted input
    #[must_use]
    pub fn is_input_error(&self) -> bool {
        matches!(
            self,
            Error::InvalidInput(_)
                | Error::Corrupted(_)
                | Error::UnsupportedFormat(_)
                | Error::ParseError(_)
        )
    }

    /// Create a parse error with context
    pub fn parse<S: Into<String>>(msg: S) -> Self {
        Error::ParseError(msg.into())
    }

    /// Create an internal error
    pub fn internal<S: Into<String>>(msg: S) -> Self {
        Error::Internal(msg.into())
    }
}

/// Extension trait for adding context to errors
pub trait ResultExt<T> {
    /// Add context to an error
    fn context<S: Into<String>>(self, msg: S) -> Result<T>;
}

impl<T, E: Into<Error>> ResultExt<T> for std::result::Result<T, E> {
    fn context<S: Into<String>>(self, msg: S) -> Result<T> {
        self.map_err(|e| {
            let inner = e.into();
            Error::Internal(format!("{}: {}", msg.into(), inner))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = Error::UnsupportedFormat("xyz".to_string());
        assert!(err.to_string().contains("xyz"));
    }

    #[test]
    fn test_error_recoverable() {
        assert!(Error::Timeout(std::time::Duration::from_secs(30)).is_recoverable());
        assert!(!Error::ParseError("test".to_string()).is_recoverable());
    }

    #[test]
    fn test_error_input() {
        assert!(Error::InvalidInput("test".to_string()).is_input_error());
        assert!(Error::Corrupted("test".to_string()).is_input_error());
        assert!(!Error::Io(io::Error::new(io::ErrorKind::NotFound, "test")).is_input_error());
    }
}

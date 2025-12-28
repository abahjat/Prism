// SPDX-License-Identifier: AGPL-3.0-only
//! External image converter utilities
//!
//! Provides optional integration with ImageMagick for converting
//! vector graphics formats (EMF, WMF, EPS) to PNG for display.

use std::io::Write;
use std::process::Command;
use tempfile::NamedTempFile;
use tracing::{debug, warn};

/// Error type for conversion operations
#[derive(Debug)]
pub enum ConversionError {
    /// `ImageMagick` is not installed or not in PATH
    NotAvailable,
    /// Failed to create temporary file
    TempFileError(std::io::Error),
    /// Conversion process failed
    ProcessFailed(String),
    /// Failed to read output file
    OutputReadError(std::io::Error),
}

impl std::fmt::Display for ConversionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAvailable => write!(f, "ImageMagick is not available"),
            Self::TempFileError(e) => write!(f, "Failed to create temp file: {e}"),
            Self::ProcessFailed(msg) => write!(f, "Conversion failed: {msg}"),
            Self::OutputReadError(e) => write!(f, "Failed to read output: {e}"),
        }
    }
}

impl std::error::Error for ConversionError {}

/// Check if `ImageMagick` is available on the system
///
/// Attempts to run `magick -version` to verify installation.
#[must_use]
pub fn is_imagemagick_available() -> bool {
    Command::new("magick")
        .arg("-version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Convert a vector graphics file to PNG using `ImageMagick`
///
/// # Arguments
/// * `data` - Raw bytes of the input file
/// * `extension` - File extension (e.g., "emf", "wmf", "eps")
///
/// # Returns
/// PNG image bytes on success, or a `ConversionError` on failure
///
/// # Errors
///
/// Returns `ConversionError::NotAvailable` if `ImageMagick` is not installed.
/// Returns `ConversionError::TempFileError` if temporary files cannot be created.
/// Returns `ConversionError::ProcessFailed` if the conversion command fails.
/// Returns `ConversionError::OutputReadError` if the output PNG cannot be read.
pub fn convert_to_png(data: &[u8], extension: &str) -> Result<Vec<u8>, ConversionError> {
    if !is_imagemagick_available() {
        return Err(ConversionError::NotAvailable);
    }

    // Create temp file for input with correct extension
    let mut input_file = NamedTempFile::with_suffix(format!(".{extension}"))
        .map_err(ConversionError::TempFileError)?;

    input_file
        .write_all(data)
        .map_err(ConversionError::TempFileError)?;

    // Create temp file for output
    let output_file = NamedTempFile::with_suffix(".png").map_err(ConversionError::TempFileError)?;

    let input_path = input_file.path();
    let output_path = output_file.path();

    debug!("Converting {} -> PNG via ImageMagick", input_path.display());

    // Run ImageMagick conversion
    // -density 150: Good quality for screen display
    // -background white: Handle transparency
    // -flatten: Merge layers
    let output = Command::new("magick")
        .arg("-density")
        .arg("150")
        .arg("-background")
        .arg("white")
        .arg(input_path)
        .arg("-flatten")
        .arg(output_path)
        .output()
        .map_err(|e| ConversionError::ProcessFailed(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("ImageMagick conversion failed: {}", stderr);
        return Err(ConversionError::ProcessFailed(stderr.to_string()));
    }

    // Read the output PNG
    std::fs::read(output_path).map_err(ConversionError::OutputReadError)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_imagemagick_availability_check() {
        // This test just verifies the function runs without panic
        let available = is_imagemagick_available();
        println!("ImageMagick available: {available}");
    }

    #[test]
    fn test_convert_fails_gracefully_without_imagemagick() {
        // If ImageMagick is not installed, should return NotAvailable
        if !is_imagemagick_available() {
            let result = convert_to_png(b"fake data", "emf");
            assert!(matches!(result, Err(ConversionError::NotAvailable)));
        }
    }
}

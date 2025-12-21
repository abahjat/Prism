//! Convert endpoint for document format conversion

use axum::{
    extract::{Multipart, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use bytes::Bytes;
use prism_core::{
    format::detect_format,
    parser::ParseContext,
    render::{RenderContext, Renderer},
};
use serde::Serialize;
use tracing::{debug, error, info, warn};

use crate::{ApiError, AppState};

/// Format detection response (fallback mode)
#[derive(Debug, Serialize)]
pub struct FormatDetectionResponse {
    /// Detected format information
    pub format: FormatInfo,
    /// Detection confidence (0.0 to 1.0)
    pub confidence: f32,
    /// Detection method used
    pub method: String,
    /// Message explaining the response
    pub message: String,
}

/// Format information
#[derive(Debug, Serialize)]
pub struct FormatInfo {
    /// MIME type
    pub mime_type: String,
    /// File extension
    pub extension: String,
    /// Format family
    pub family: String,
    /// Format name
    pub name: String,
    /// Whether this is a container format
    pub is_container: bool,
}

/// Convert endpoint handler
///
/// Accepts a file upload and attempts to convert it to the output format.
/// If no parser is available and fallback mode is enabled, returns format detection info.
pub async fn convert(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    debug!("Received convert request");

    // Extract file from multipart
    let (filename, file_data) = extract_file(&mut multipart).await?;
    let file_size = file_data.len();

    info!(
        "Processing file: {:?}, size: {} bytes",
        filename, file_size
    );

    // Validate file size
    if file_size > state.config.max_file_size {
        return Err(ApiError::BadRequest(format!(
            "File size {} exceeds maximum allowed size {}",
            file_size, state.config.max_file_size
        )));
    }

    // Detect format
    let format_result = detect_format(&file_data, filename.as_deref()).ok_or_else(|| {
        ApiError::UnsupportedMediaType("Unable to detect file format".to_string())
    })?;

    debug!(
        "Detected format: {} (confidence: {:.2}%), MIME: {}",
        format_result.format.name,
        format_result.confidence * 100.0,
        format_result.format.mime_type
    );

    // Check if parser exists
    let has_parser = state.parser_registry.has_parser(&format_result.format);
    debug!("Parser available for {}: {}", format_result.format.mime_type, has_parser);

    match state.parser_registry.get_parser(&format_result.format) {
        Some(parser) => {
            // Parser available - perform conversion
            info!(
                "Parser found for format: {}",
                format_result.format.mime_type
            );

            // Parse document
            let parse_context = ParseContext {
                format: format_result.format.clone(),
                filename: filename.clone(),
                size: file_size,
                options: Default::default(),
            };

            let document = parser
                .parse(Bytes::from(file_data.clone()), parse_context)
                .await
                .map_err(|e| {
                    error!("Parse error: {}", e);
                    ApiError::InternalServerError(format!("Failed to parse document: {}", e))
                })?;

            debug!("Document parsed successfully, pages: {}", document.page_count());

            // Render to HTML
            let render_context = RenderContext {
                options: Default::default(),
                filename: filename.clone(),
            };

            let html_bytes = state
                .html_renderer
                .render(&document, render_context)
                .await
                .map_err(|e| {
                    error!("Render error: {}", e);
                    ApiError::InternalServerError(format!("Failed to render document: {}", e))
                })?;

            info!("Document rendered successfully to HTML");

            // Return HTML response
            Ok((
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                html_bytes,
            )
                .into_response())
        }
        None => {
            // No parser available
            if state.config.enable_fallback {
                // Fallback mode - return format detection info
                warn!(
                    "No parser available for format: {}, returning detection info",
                    format_result.format.mime_type
                );

                let response = FormatDetectionResponse {
                    format: FormatInfo {
                        mime_type: format_result.format.mime_type.clone(),
                        extension: format_result.format.extension.clone(),
                        family: format!("{:?}", format_result.format.family),
                        name: format_result.format.name.clone(),
                        is_container: format_result.format.is_container,
                    },
                    confidence: format_result.confidence as f32,
                    method: format!("{:?}", format_result.method),
                    message: format!(
                        "Format detected as {} but no parser is available. Returning format detection information.",
                        format_result.format.name
                    ),
                };

                Ok(Json(response).into_response())
            } else {
                // Fallback disabled - return error
                Err(ApiError::NotImplemented(format!(
                    "No parser available for format: {}",
                    format_result.format.name
                )))
            }
        }
    }
}

/// Extract file from multipart form data
async fn extract_file(multipart: &mut Multipart) -> Result<(Option<String>, Vec<u8>), ApiError> {
    while let Some(field) = multipart.next_field().await.map_err(|e| {
        ApiError::BadRequest(format!("Failed to read multipart field: {}", e))
    })? {
        let name = field.name().unwrap_or("").to_string();

        if name == "file" {
            let filename = field.file_name().map(|s| s.to_string());
            let data = field.bytes().await.map_err(|e| {
                ApiError::BadRequest(format!("Failed to read file data: {}", e))
            })?;

            debug!(
                "Extracted file: {:?}, size: {} bytes",
                filename,
                data.len()
            );

            return Ok((filename, data.to_vec()));
        }
    }

    Err(ApiError::BadRequest(
        "No file field found in multipart form".to_string(),
    ))
}

// SPDX-License-Identifier: AGPL-3.0-only
//! Document conversion and Phase 4 review/render/export endpoint handlers.
//!
//! Every handler enforces `max_file_size` and the concurrency semaphore, and routes all
//! document parsing, rendering, reporting, family-tree extraction, and destructive redaction
//! through the configured [`prism_sandbox::SandboxExecutor`].

use axum::{
    Json,
    extract::{Multipart, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use prism_core::format::detect_format;
use prism_review::cache::{ArtifactCache, CacheArtifactKind, CacheKey};
use prism_sandbox::{ExportOutputFormat, RenderOutputFormat, SandboxJob};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::{ApiError, AppState};

/// Format detection response (fallback mode for `/api/convert`).
#[derive(Debug, Serialize)]
pub struct FormatDetectionResponse {
    /// Detected format information.
    pub format: FormatInfo,
    /// Detection confidence (`0.0` to `1.0`).
    pub confidence: f32,
    /// Detection method used.
    pub method: String,
    /// Message explaining the response.
    pub message: String,
    /// Preview of file content (up to 1000 characters).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// Format information summary.
#[derive(Debug, Serialize)]
pub struct FormatInfo {
    /// MIME type.
    pub mime_type: String,
    /// File extension.
    pub extension: String,
    /// Format family.
    pub family: String,
    /// Format name.
    pub name: String,
    /// Whether this is a container format.
    pub is_container: bool,
}

/// Query parameters for `POST /api/render`.
#[derive(Debug, Clone, Deserialize)]
pub struct RenderQuery {
    /// 1-based page number to render (defaults to `1`).
    #[serde(default = "default_page_one")]
    pub page: u32,
    /// Output format: `"svg"` (default) or `"png"`.
    #[serde(default = "default_svg_format")]
    pub format: String,
    /// Rasterization DPI for `"png"` output (defaults to `150.0`).
    #[serde(default = "default_dpi")]
    pub dpi: f32,
}

const fn default_page_one() -> u32 {
    1
}

fn default_svg_format() -> String {
    "svg".to_string()
}

fn default_pdf_format() -> String {
    "pdf".to_string()
}

const fn default_dpi() -> f32 {
    150.0
}

/// Query parameters for `POST /api/export`.
#[derive(Debug, Clone, Deserialize)]
pub struct ExportQuery {
    /// Output format: `"pdf"` (default) or `"tiff"`.
    #[serde(default = "default_pdf_format")]
    pub format: String,
    /// Rasterization DPI for `"tiff"` output (defaults to `150.0`).
    #[serde(default = "default_dpi")]
    pub dpi: f32,
}

/// Extracted multipart form payload.
struct ExtractedUpload {
    filename: Option<String>,
    file_data: Vec<u8>,
    sidecar_json: Option<String>,
}

/// Convert endpoint (`POST /api/convert`): renders an uploaded document to self-contained HTML5
/// via the sandbox worker pool.
///
/// # Errors
///
/// Returns [`ApiError`] if the upload is missing, exceeds `max_file_size`, or fails in the sandbox.
pub async fn convert(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    let _permit = state.acquire_permit().await?;
    let upload = extract_upload(&mut multipart, state.config.max_file_size).await?;

    info!(
        "Processing /api/convert: {:?}, size: {} bytes",
        upload.filename,
        upload.file_data.len()
    );

    let Some(format_result) = detect_format(&upload.file_data, upload.filename.as_deref()) else {
        warn!("Unable to detect file format, returning preview");
        let preview = extract_file_preview(&upload.file_data);
        let response = FormatDetectionResponse {
            format: FormatInfo {
                mime_type: "application/octet-stream".to_string(),
                extension: upload
                    .filename
                    .as_ref()
                    .and_then(|f| f.rsplit('.').next())
                    .unwrap_or("unknown")
                    .to_string(),
                family: "Unknown".to_string(),
                name: "Unknown Format".to_string(),
                is_container: false,
            },
            confidence: 0.0,
            method: "None".to_string(),
            message: "Unable to detect file format. Showing file content preview.".to_string(),
            preview,
        };
        return Ok(Json(response).into_response());
    };

    if state
        .parser_registry
        .get_parser_for_data(&format_result.format, &upload.file_data)
        .is_some()
    {
        let out = state
            .run_job(SandboxJob::RenderHtml {
                filename: upload.filename,
                bytes: upload.file_data,
            })
            .await?;

        Ok((
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            out.data,
        )
            .into_response())
    } else if state.config.enable_fallback {
        let preview = extract_file_preview(&upload.file_data);
        #[allow(clippy::cast_possible_truncation)]
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
            preview,
        };
        Ok(Json(response).into_response())
    } else {
        Err(ApiError::NotImplemented(format!(
            "No parser available for format: {}",
            format_result.format.name
        )))
    }
}

/// Options that change a cached result besides the bytes: the filename steers format
/// detection, so the same bytes uploaded as `.csv` and `.txt` must not share an entry.
fn cache_options(upload: &ExtractedUpload) -> String {
    serde_json::json!({ "filename": upload.filename }).to_string()
}

/// Render a single page (`POST /api/render?page=1&format=svg|png&dpi=150`) via the sandbox pool.
///
/// # Errors
///
/// Returns [`ApiError`] on invalid parameters, oversized payload, or sandbox failure.
pub async fn render_page_handler(
    State(state): State<AppState>,
    Query(query): Query<RenderQuery>,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    let _permit = state.acquire_permit().await?;
    if query.page == 0 {
        return Err(ApiError::BadRequest(
            "Query parameter 'page' is 1-based and must be >= 1".to_string(),
        ));
    }
    let page_index = query.page - 1;
    let render_format = match query.format.to_ascii_lowercase().as_str() {
        "svg" => RenderOutputFormat::Svg,
        "png" => RenderOutputFormat::Png,
        other => {
            return Err(ApiError::BadRequest(format!(
                "Unsupported render format '{other}' (expected 'svg' or 'png')"
            )));
        }
    };

    let upload = extract_upload(&mut multipart, state.config.max_file_size).await?;

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let cache_kind = match render_format {
        RenderOutputFormat::Svg => CacheArtifactKind::PageSvg { page_index },
        RenderOutputFormat::Png => CacheArtifactKind::PagePng {
            page_index,
            dpi_milli: (query.dpi * 1000.0).round() as u32,
        },
    };
    let cache_key = CacheKey::new(&upload.file_data, &cache_options(&upload), cache_kind);
    if let Some(ref cache) = state.cache
        && let Ok(Some(cached)) = cache.get(&cache_key)
    {
        let content_type = match render_format {
            RenderOutputFormat::Svg => "image/svg+xml; charset=utf-8",
            RenderOutputFormat::Png => "image/png",
        };
        return Ok((
            StatusCode::OK,
            [(header::CONTENT_TYPE, content_type)],
            cached,
        )
            .into_response());
    }

    let out = state
        .run_job(SandboxJob::RenderPage {
            filename: upload.filename,
            bytes: upload.file_data,
            page_index,
            format: render_format,
            dpi: query.dpi,
        })
        .await?;

    if let Some(ref cache) = state.cache {
        let _ = cache.put(&cache_key, &out.data);
    }

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, out.content_type)],
        out.data,
    )
        .into_response())
}

/// Extract plain UTF-8 text (`POST /api/text`) via the sandbox pool.
///
/// # Errors
///
/// Returns [`ApiError`] on oversized payload or sandbox failure.
pub async fn extract_text_handler(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    let _permit = state.acquire_permit().await?;
    let upload = extract_upload(&mut multipart, state.config.max_file_size).await?;

    let cache_key = CacheKey::new(
        &upload.file_data,
        &cache_options(&upload),
        CacheArtifactKind::ExtractedText,
    );
    if let Some(ref cache) = state.cache
        && let Ok(Some(cached)) = cache.get(&cache_key)
    {
        return Ok((
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            cached,
        )
            .into_response());
    }

    let out = state
        .run_job(SandboxJob::ExtractText {
            filename: upload.filename,
            bytes: upload.file_data,
        })
        .await?;

    if let Some(ref cache) = state.cache {
        let _ = cache.put(&cache_key, &out.data);
    }

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        out.data,
    )
        .into_response())
}

/// Generate the hidden-content report (`POST /api/report`) via the sandbox pool.
///
/// # Errors
///
/// Returns [`ApiError`] on oversized payload or sandbox failure.
pub async fn report_handler(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    let _permit = state.acquire_permit().await?;
    let upload = extract_upload(&mut multipart, state.config.max_file_size).await?;

    let cache_key = CacheKey::new(
        &upload.file_data,
        &cache_options(&upload),
        CacheArtifactKind::HiddenContentReport,
    );
    if let Some(ref cache) = state.cache
        && let Ok(Some(cached)) = cache.get(&cache_key)
    {
        return Ok((
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            cached,
        )
            .into_response());
    }

    let out = state
        .run_job(SandboxJob::HiddenContentReport {
            filename: upload.filename,
            bytes: upload.file_data,
        })
        .await?;

    if let Some(ref cache) = state.cache {
        let _ = cache.put(&cache_key, &out.data);
    }

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        out.data,
    )
        .into_response())
}

/// Build the container/embedded-object family tree (`POST /api/tree`) via the sandbox pool.
///
/// # Errors
///
/// Returns [`ApiError`] on oversized payload or sandbox failure.
pub async fn tree_handler(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    let _permit = state.acquire_permit().await?;
    let upload = extract_upload(&mut multipart, state.config.max_file_size).await?;

    let cache_key = CacheKey::new(
        &upload.file_data,
        &cache_options(&upload),
        CacheArtifactKind::FamilyTree,
    );
    if let Some(ref cache) = state.cache
        && let Ok(Some(cached)) = cache.get(&cache_key)
    {
        return Ok((
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            cached,
        )
            .into_response());
    }

    let out = state
        .run_job(SandboxJob::FamilyTree {
            filename: upload.filename,
            bytes: upload.file_data,
        })
        .await?;

    if let Some(ref cache) = state.cache {
        let _ = cache.put(&cache_key, &out.data);
    }

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        out.data,
    )
        .into_response())
}

/// Export a document to PDF or multi-page TIFF (`POST /api/export?format=pdf|tiff&dpi=150`),
/// optionally applying and verifying destructive redactions from a `redactions` or `sidecar`
/// multipart form field.
///
/// # Errors
///
/// Returns [`ApiError`] on invalid parameters, redaction verification failure, or sandbox error.
pub async fn export_handler(
    State(state): State<AppState>,
    Query(query): Query<ExportQuery>,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    let _permit = state.acquire_permit().await?;
    let export_format = match query.format.to_ascii_lowercase().as_str() {
        "pdf" => ExportOutputFormat::Pdf,
        "tiff" | "tif" => ExportOutputFormat::Tiff,
        other => {
            return Err(ApiError::BadRequest(format!(
                "Unsupported export format '{other}' (expected 'pdf' or 'tiff')"
            )));
        }
    };

    let upload = extract_upload(&mut multipart, state.config.max_file_size).await?;

    let out = state
        .run_job(SandboxJob::ExportDocument {
            filename: upload.filename,
            bytes: upload.file_data,
            format: export_format,
            dpi: query.dpi,
            sidecar_json: upload.sidecar_json,
        })
        .await?;

    let mut resp = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, out.content_type)],
        out.data,
    )
        .into_response();

    if out.redaction_log_json.is_some()
        && let Ok(val) = axum::http::HeaderValue::from_str("true")
    {
        resp.headers_mut().insert("x-prism-redactions-applied", val);
    }

    Ok(resp)
}

async fn extract_upload(
    multipart: &mut Multipart,
    max_file_size: usize,
) -> Result<ExtractedUpload, ApiError> {
    let mut filename = None;
    let mut file_data: Option<Vec<u8>> = None;
    let mut sidecar_json = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("Failed to read multipart field: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            filename = field.file_name().map(str::to_string);
            let data = field
                .bytes()
                .await
                .map_err(|e| ApiError::BadRequest(format!("Failed to read file data: {e}")))?;
            if data.len() > max_file_size {
                return Err(ApiError::PayloadTooLarge(format!(
                    "File size {} bytes exceeds maximum allowed size {} bytes",
                    data.len(),
                    max_file_size
                )));
            }
            debug!("Extracted file: {:?}, size: {} bytes", filename, data.len());
            file_data = Some(data.to_vec());
        } else if name == "redactions" || name == "sidecar" {
            let data = field.bytes().await.map_err(|e| {
                ApiError::BadRequest(format!("Failed to read redactions sidecar field: {e}"))
            })?;
            let text = String::from_utf8(data.to_vec()).map_err(|e| {
                ApiError::BadRequest(format!("Redactions sidecar must be valid UTF-8 JSON: {e}"))
            })?;
            sidecar_json = Some(text);
        }
    }

    let Some(file_data) = file_data else {
        return Err(ApiError::BadRequest(
            "No 'file' field found in multipart form".to_string(),
        ));
    };

    Ok(ExtractedUpload {
        filename,
        file_data,
        sidecar_json,
    })
}

fn extract_file_preview(data: &[u8]) -> Option<String> {
    const MAX_PREVIEW_CHARS: usize = 1000;
    const MAX_HEX_BYTES: usize = 256;

    if data.is_empty() {
        return None;
    }

    if let Ok(text) = std::str::from_utf8(data) {
        let preview: String = text
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .take(MAX_PREVIEW_CHARS)
            .collect();
        if !preview.trim().is_empty() {
            return Some(preview);
        }
    }

    let mut preview = String::new();
    let mut consecutive_printable = 0;
    let mut buffer = String::new();

    for &byte in data {
        if (32..127).contains(&byte) || byte == b'\n' || byte == b'\t' {
            buffer.push(byte as char);
            consecutive_printable += 1;
        } else {
            if consecutive_printable >= 4 {
                preview.push_str(&buffer);
                preview.push(' ');
            }
            buffer.clear();
            consecutive_printable = 0;
        }
        if preview.len() >= MAX_PREVIEW_CHARS {
            break;
        }
    }

    if consecutive_printable >= 4 && preview.len() < MAX_PREVIEW_CHARS {
        preview.push_str(&buffer);
    }

    let preview = preview.trim().to_string();
    if !preview.is_empty() {
        return Some(preview.chars().take(MAX_PREVIEW_CHARS).collect());
    }

    let bytes_to_show = data.len().min(MAX_HEX_BYTES);
    let mut hex_preview = String::from("Binary file - Hex dump:\n");
    for (i, chunk) in data[..bytes_to_show].chunks(16).enumerate() {
        use std::fmt::Write as _;
        let _ = write!(hex_preview, "{:04X}: ", i * 16);
        for byte in chunk {
            let _ = write!(hex_preview, "{byte:02X} ");
        }
        for _ in 0..(16 - chunk.len()) {
            hex_preview.push_str("   ");
        }
        hex_preview.push_str(" | ");
        for &byte in chunk {
            if (32..127).contains(&byte) {
                hex_preview.push(byte as char);
            } else {
                hex_preview.push('.');
            }
        }
        hex_preview.push('\n');
    }
    if data.len() > bytes_to_show {
        use std::fmt::Write as _;
        let _ = write!(
            hex_preview,
            "... ({} more bytes)",
            data.len() - bytes_to_show
        );
    }
    Some(hex_preview)
}

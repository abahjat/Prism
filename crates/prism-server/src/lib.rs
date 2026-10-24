// SPDX-License-Identifier: AGPL-3.0-only
//! # Prism Server Library (`prism-server`)
//!
//! Hardened REST API server for Prism document processing. Routes all parsing, rendering,
//! forensic hidden-content reporting, family-tree inspection, and destructive redaction
//! through the `prism-sandbox` worker pool (`SandboxManager`) with per-request size limits,
//! concurrency limits, and optional content-addressed caching (`FilesystemCache`).

#![warn(missing_docs)]
#![warn(clippy::all)]
#![warn(clippy::pedantic)]
#![allow(
    clippy::double_must_use,
    clippy::module_name_repetitions,
    clippy::too_many_lines
)]

/// Server configuration and CLI flags.
pub mod config;
/// Document conversion, rendering, reporting, tree, and redacted export endpoints.
pub mod convert;

use axum::http::{HeaderValue, Method, header};
use axum::{
    Router,
    extract::{DefaultBodyLimit, Json},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use prism_parsers::ParserRegistry;
use prism_review::cache::FilesystemCache;
use prism_sandbox::{
    InProcessExecutor, SandboxConfig, SandboxError, SandboxExecutor, SandboxJob, SandboxManager,
    SandboxOutput,
};
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::ServeDir;
use tracing::info;

pub use config::{ServerArgs, ServerConfig};

/// Prism server version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Shared application state for the Axum router.
#[derive(Clone)]
pub struct AppState {
    /// Parser registry for format support inspection.
    pub parser_registry: Arc<ParserRegistry>,
    /// Process-isolated worker pool (`SandboxManager`) or in-process executor (`InProcessExecutor`).
    pub executor: Arc<dyn SandboxExecutor>,
    /// Optional content-addressed artifact cache.
    pub cache: Option<Arc<FilesystemCache>>,
    /// Concurrency limit semaphore.
    pub semaphore: Arc<Semaphore>,
    /// Server configuration.
    pub config: Arc<ServerConfig>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::with_config(ServerConfig::default())
    }
}

impl AppState {
    /// Create a new [`AppState`] with default [`ServerConfig`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(ServerConfig::default())
    }

    /// Create a new [`AppState`] from an explicit [`ServerConfig`].
    #[must_use]
    pub fn with_config(config: ServerConfig) -> Self {
        let registry = build_full_registry();
        let executor: Arc<dyn SandboxExecutor> = if config.in_process {
            Arc::new(InProcessExecutor::default())
        } else {
            let sb_cfg = SandboxConfig {
                pool_size: config.worker_pool_size.max(1),
                max_memory: config.max_memory_bytes,
                max_execution_time: Duration::from_secs(config.timeout_seconds.max(1)),
                ..SandboxConfig::default()
            };
            Arc::new(SandboxManager::new(sb_cfg))
        };

        let cache = config
            .cache_dir
            .as_ref()
            .and_then(|dir| FilesystemCache::new(dir, config.cache_max_bytes).ok())
            .map(Arc::new);

        let semaphore = Arc::new(Semaphore::new(config.max_concurrent_requests.max(1)));

        Self {
            parser_registry: Arc::new(registry),
            executor,
            cache,
            semaphore,
            config: Arc::new(config),
        }
    }

    /// Acquire a concurrency permit for a document-processing request, returning `429 Too Many Requests`
    /// if the server's concurrency budget is exhausted.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::TooManyRequests`] when all concurrency slots are occupied.
    pub async fn acquire_permit(&self) -> Result<OwnedSemaphorePermit, ApiError> {
        let timeout = Duration::from_secs(self.config.timeout_seconds.max(1));
        tokio::time::timeout(timeout, self.semaphore.clone().acquire_owned())
            .await
            .map_err(|_| {
                ApiError::TooManyRequests(format!(
                    "Server concurrency limit ({}) exceeded",
                    self.config.max_concurrent_requests
                ))
            })?
            .map_err(|_| ApiError::InternalServerError("Concurrency semaphore closed".to_string()))
    }

    /// Execute a [`SandboxJob`] on a blocking thread via the configured [`SandboxExecutor`],
    /// mapping [`SandboxError`]s to structured [`ApiError`]s.
    ///
    /// # Errors
    ///
    /// Returns a structured [`ApiError`] on timeout, OOM, crash, or parse/verification error.
    pub async fn run_job(&self, job: SandboxJob) -> Result<SandboxOutput, ApiError> {
        let exec = self.executor.clone();
        let join_res = tokio::task::spawn_blocking(move || exec.execute(job))
            .await
            .map_err(|e| ApiError::InternalServerError(format!("Worker task join error: {e}")))?;

        join_res.map_err(|err| match err {
            SandboxError::Timeout(dur) => {
                ApiError::GatewayTimeout(format!("Worker exceeded wall-clock timeout ({dur:?})"))
            }
            SandboxError::MemoryLimit { limit_bytes } => ApiError::ServiceUnavailable(format!(
                "Worker exceeded memory limit ({limit_bytes} bytes)"
            )),
            SandboxError::Crashed { exit_code } => ApiError::ServiceUnavailable(format!(
                "Worker process terminated abnormally (exit code: {exit_code:?})"
            )),
            SandboxError::OutputTooLarge { size, limit } => ApiError::PayloadTooLarge(format!(
                "Worker output ({size} bytes) exceeded maximum allowed output ({limit} bytes)"
            )),
            SandboxError::ProtocolError(msg) => {
                ApiError::ServiceUnavailable(format!("Worker protocol error: {msg}"))
            }
            SandboxError::TaskFailed(msg) => {
                if msg.contains("Redaction verification failed") {
                    ApiError::UnprocessableEntity(msg)
                } else {
                    ApiError::BadRequest(msg)
                }
            }
        })
    }
}

fn build_full_registry() -> ParserRegistry {
    let mut registry = ParserRegistry::new();

    registry.register(Arc::new(prism_parsers::PdfParser::new()));
    registry.register(Arc::new(prism_parsers::PngParser::new()));
    registry.register(Arc::new(prism_parsers::JpegParser::new()));
    registry.register(Arc::new(prism_parsers::TiffParser::new()));
    registry.register(Arc::new(prism_parsers::GifParser::new()));
    registry.register(Arc::new(prism_parsers::WebpParser::new()));
    registry.register(Arc::new(prism_parsers::BmpParser::new()));
    registry.register(Arc::new(prism_parsers::SvgParser::new()));
    registry.register(Arc::new(prism_parsers::EmfParser::new()));
    registry.register(Arc::new(prism_parsers::EmzParser::new()));
    registry.register(Arc::new(prism_parsers::EpsParser::new()));
    registry.register(Arc::new(prism_parsers::WmfParser::new()));
    registry.register(Arc::new(prism_parsers::IcoParser::new()));
    registry.register(Arc::new(prism_parsers::TgaParser::new()));
    registry.register(Arc::new(prism_parsers::SvgzParser::new()));
    registry.register(Arc::new(prism_parsers::DocxParser::new()));
    registry.register(Arc::new(prism_parsers::PptxParser::new()));
    registry.register(Arc::new(prism_parsers::XlsxParser::new()));
    registry.register(Arc::new(prism_parsers::DocParser::new()));
    registry.register(Arc::new(prism_parsers::PptParser::new()));
    registry.register(Arc::new(prism_parsers::XlsParser::new()));
    registry.register(Arc::new(prism_parsers::OdtParser::new()));
    registry.register(Arc::new(prism_parsers::OdsParser::new()));
    registry.register(Arc::new(prism_parsers::OdpParser::new()));
    registry.register(Arc::new(prism_parsers::OdgParser::new()));
    registry.register(Arc::new(prism_parsers::OneNoteParser::new()));
    registry.register(Arc::new(prism_parsers::VsdxParser::new()));
    registry.register(Arc::new(prism_parsers::MppParser::new()));
    registry.register(Arc::new(prism_parsers::XpsParser::new()));
    registry.register(Arc::new(prism_parsers::TextParser::new()));
    registry.register(Arc::new(prism_parsers::EpubParser::new()));
    registry.register(Arc::new(prism_parsers::HtmlParser::new()));
    registry.register(Arc::new(prism_parsers::JsonParser::new()));
    registry.register(Arc::new(prism_parsers::XmlParser::new()));
    registry.register(Arc::new(prism_parsers::CsvParser::new()));
    registry.register(Arc::new(prism_parsers::MarkdownParser::new()));
    registry.register(Arc::new(prism_parsers::LogParser::new()));
    registry.register(Arc::new(prism_parsers::RtfParser::new()));
    registry.register(Arc::new(prism_parsers::CodeParser::new(
        prism_core::format::Format::rust(),
    )));
    registry.register(Arc::new(prism_parsers::CodeParser::new(
        prism_core::format::Format::python(),
    )));
    registry.register(Arc::new(prism_parsers::CodeParser::new(
        prism_core::format::Format::javascript(),
    )));
    registry.register(Arc::new(prism_parsers::CodeParser::new(
        prism_core::format::Format::typescript(),
    )));
    registry.register(Arc::new(prism_parsers::CodeParser::new(
        prism_core::format::Format::c(),
    )));
    registry.register(Arc::new(prism_parsers::CodeParser::new(
        prism_core::format::Format::cpp(),
    )));
    registry.register(Arc::new(prism_parsers::CodeParser::new(
        prism_core::format::Format::css(),
    )));
    registry.register(Arc::new(prism_parsers::EmlParser::new()));
    registry.register(Arc::new(prism_parsers::MsgParser::new()));
    registry.register(Arc::new(prism_parsers::MboxParser::new()));
    registry.register(Arc::new(prism_parsers::VcfParser::new()));
    registry.register(Arc::new(prism_parsers::IcsParser::new()));
    registry.register(Arc::new(prism_parsers::MhtParser::new()));
    registry.register(Arc::new(prism_parsers::DxfParser::new()));
    registry.register(Arc::new(prism_parsers::ArchiveParser::new(
        prism_core::format::Format::zip(),
    )));
    registry.register(Arc::new(prism_parsers::ArchiveParser::new(
        prism_core::format::Format::tar(),
    )));
    registry.register(Arc::new(prism_parsers::ArchiveParser::new(
        prism_core::format::Format::gzip(),
    )));

    registry
}

/// Health check response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// Service status (`"ok"`).
    pub status: String,
    /// Server version string.
    pub version: String,
}

/// Structured JSON error response returned by all API endpoints on failure.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    /// HTTP status description.
    pub error: String,
    /// Machine-readable error category code.
    pub code: String,
    /// Human-readable diagnostic message.
    pub message: String,
}

/// Structured API error variants mapped to HTTP status codes.
#[derive(Debug)]
pub enum ApiError {
    /// `400 Bad Request`
    BadRequest(String),
    /// `413 Payload Too Large`
    PayloadTooLarge(String),
    /// `415 Unsupported Media Type`
    UnsupportedMediaType(String),
    /// `422 Unprocessable Entity` (e.g. destructive redaction verification failed)
    UnprocessableEntity(String),
    /// `429 Too Many Requests` (concurrency limit reached)
    TooManyRequests(String),
    /// `501 Not Implemented`
    NotImplemented(String),
    /// `503 Service Unavailable` (worker memory limit or worker crash)
    ServiceUnavailable(String),
    /// `504 Gateway Timeout` (worker wall-clock timeout)
    GatewayTimeout(String),
    /// `500 Internal Server Error`
    InternalServerError(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::BadRequest(msg) => (StatusCode::BAD_REQUEST, "bad_request", msg),
            Self::PayloadTooLarge(msg) => (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large", msg),
            Self::UnsupportedMediaType(msg) => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_media_type",
                msg,
            ),
            Self::UnprocessableEntity(msg) => {
                (StatusCode::UNPROCESSABLE_ENTITY, "verification_failed", msg)
            }
            Self::TooManyRequests(msg) => (
                StatusCode::TOO_MANY_REQUESTS,
                "concurrency_limit_exceeded",
                msg,
            ),
            Self::NotImplemented(msg) => (StatusCode::NOT_IMPLEMENTED, "not_implemented", msg),
            Self::ServiceUnavailable(msg) => {
                (StatusCode::SERVICE_UNAVAILABLE, "sandbox_violation", msg)
            }
            Self::GatewayTimeout(msg) => (StatusCode::GATEWAY_TIMEOUT, "sandbox_timeout", msg),
            Self::InternalServerError(msg) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error", msg)
            }
        };

        let body = Json(ErrorResponse {
            error: status.to_string(),
            code: code.to_string(),
            message,
        });

        (status, body).into_response()
    }
}

/// Health check endpoint (`GET /api/health`).
pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: VERSION.to_string(),
    })
}

/// Version endpoint (`GET /api/version`).
pub async fn version() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "server": VERSION,
        "engine_version": prism_core::ENGINE_VERSION,
        "commit": prism_core::GIT_HASH,
        "full_version": prism_core::FULL_VERSION,
        "core": prism_core::VERSION,
        "parsers": prism_parsers::VERSION,
        "render": prism_render::VERSION,
        "sandbox": prism_sandbox::VERSION,
    }))
}

/// Build the `/api` router with all Phase 4 endpoints and configured limits.
pub fn build_api_router(state: AppState) -> Router {
    let body_limit = state.config.max_file_size.saturating_add(4 * 1024 * 1024);
    Router::new()
        .route("/health", get(health))
        .route("/version", get(version))
        .route("/convert", post(convert::convert))
        .route("/render", post(convert::render_page_handler))
        .route("/text", post(convert::extract_text_handler))
        .route("/report", post(convert::report_handler))
        .route("/tree", post(convert::tree_handler))
        .route("/export", post(convert::export_handler))
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(state)
}

/// Build the full application router nesting `/api` and static viewer assets.
pub fn build_app_router(state: AppState, cors_origins: &str) -> Router {
    let api_router = build_api_router(state);
    let cors_layer = build_cors_layer(cors_origins);
    Router::new()
        .nest("/api", api_router)
        .nest_service("/", ServeDir::new("web-viewer"))
        .layer(cors_layer)
}

/// Build CORS layer based on configuration.
#[must_use]
pub fn build_cors_layer(origins: &str) -> CorsLayer {
    let origins = origins.trim();

    if origins == "*" {
        info!("CORS: Permissive mode enabled (all origins allowed)");
        return CorsLayer::permissive();
    }

    if origins.is_empty() {
        CorsLayer::new()
            .allow_origin(AllowOrigin::predicate(|origin: &HeaderValue, _| {
                if let Ok(origin_str) = origin.to_str() {
                    origin_str == "https://documain.ai"
                        || origin_str == "http://documain.ai"
                        || origin_str.ends_with(".documain.ai")
                        || origin_str.starts_with("http://localhost:")
                        || origin_str.starts_with("http://127.0.0.1:")
                        || origin_str == "http://localhost"
                        || origin_str == "http://127.0.0.1"
                } else {
                    false
                }
            }))
            .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
            .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION, header::ACCEPT])
            .expose_headers([header::CONTENT_TYPE])
            .allow_credentials(true)
    } else {
        let allowed_origins: Vec<HeaderValue> = origins
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();

        CorsLayer::new()
            .allow_origin(allowed_origins)
            .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
            .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION, header::ACCEPT])
            .expose_headers([header::CONTENT_TYPE])
            .allow_credentials(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use prism_core::review::SourceRef;
    use prism_review::sidecar::{
        Annotation, AnnotationAnchor, AnnotationKind, AnnotationSidecar, RedactionTarget,
        TextAnchor,
    };
    use tower::Service;

    fn make_multipart_body(
        boundary: &str,
        filename: &str,
        file_bytes: &[u8],
        sidecar_json: Option<&str>,
    ) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(file_bytes);
        body.extend_from_slice(b"\r\n");

        if let Some(sc) = sidecar_json {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            body.extend_from_slice(
                b"Content-Disposition: form-data; name=\"redactions\"\r\nContent-Type: application/json\r\n\r\n",
            );
            body.extend_from_slice(sc.as_bytes());
            body.extend_from_slice(b"\r\n");
        }

        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        body
    }

    #[tokio::test]
    async fn server_endpoints_and_limits_work_through_executor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cfg = ServerConfig {
            max_file_size: 1024,
            in_process: true,
            cache_dir: Some(temp.path().join("cache")),
            ..ServerConfig::default()
        };
        let state = AppState::with_config(cfg);
        let mut router = build_api_router(state);

        // 1. /health
        let resp_health = router
            .call(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_health.status(), StatusCode::OK);

        // 2. /text, /report, /tree, /render, /export (with redactions)
        let doc_bytes = b"Server confidential token TOKEN-7741-ALPHA in document body.";
        let boundary = "----PrismTestBoundary123";
        let mp_body = make_multipart_body(boundary, "note.txt", doc_bytes, None);
        let ct = format!("multipart/form-data; boundary={boundary}");

        let resp_text = router
            .call(
                Request::builder()
                    .method("POST")
                    .uri("/text")
                    .header("content-type", &ct)
                    .body(Body::from(mp_body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_text.status(), StatusCode::OK);
        let text_bytes = axum::body::to_bytes(resp_text.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&text_bytes).contains("TOKEN-7741-ALPHA"));

        let resp_report = router
            .call(
                Request::builder()
                    .method("POST")
                    .uri("/report")
                    .header("content-type", &ct)
                    .body(Body::from(mp_body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_report.status(), StatusCode::OK);

        let resp_tree = router
            .call(
                Request::builder()
                    .method("POST")
                    .uri("/tree")
                    .header("content-type", &ct)
                    .body(Body::from(mp_body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_tree.status(), StatusCode::OK);

        let resp_render = router
            .call(
                Request::builder()
                    .method("POST")
                    .uri("/render?page=1&format=svg")
                    .header("content-type", &ct)
                    .body(Body::from(mp_body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_render.status(), StatusCode::OK);

        // Export with destructive redactions
        let mut sidecar = AnnotationSidecar::new(doc_bytes);
        sidecar.add(Annotation {
            id: "r1".to_string(),
            kind: AnnotationKind::Redaction {
                reason_code: "PII".to_string(),
                label: "REDACTED".to_string(),
                fill_color: None,
            },
            anchor: AnnotationAnchor {
                target: RedactionTarget::TextRange,
                page_index: 0,
                rects_pt: Vec::new(),
                text_anchor: Some(TextAnchor {
                    start_source: SourceRef::default(),
                    end_source: SourceRef::default(),
                    quoted_text: "TOKEN-7741-ALPHA".to_string(),
                    prefix: String::new(),
                    suffix: String::new(),
                }),
            },
            author: None,
            created_at: None,
            proposed: false,
        });
        let sc_json = sidecar.to_json_deterministic().unwrap();
        let mp_export = make_multipart_body(boundary, "note.txt", doc_bytes, Some(&sc_json));
        let resp_export = router
            .call(
                Request::builder()
                    .method("POST")
                    .uri("/export?format=pdf")
                    .header("content-type", &ct)
                    .body(Body::from(mp_export))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_export.status(), StatusCode::OK);
        assert_eq!(
            resp_export
                .headers()
                .get("x-prism-redactions-applied")
                .and_then(|v| v.to_str().ok()),
            Some("true")
        );

        // 3. Oversized file (> 1024 bytes) rejected with 413 Payload Too Large
        let big_bytes = vec![b'A'; 2048];
        let mp_big = make_multipart_body(boundary, "big.txt", &big_bytes, None);
        let resp_big = router
            .call(
                Request::builder()
                    .method("POST")
                    .uri("/text")
                    .header("content-type", &ct)
                    .body(Body::from(mp_big))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_big.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}

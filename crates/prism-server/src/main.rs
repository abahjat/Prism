//! # Prism Server
//!
//! REST API server for Prism document processing.
//!
//! This is the main entry point for the Prism HTTP server.

mod config;
mod convert;

use axum::{
    extract::{DefaultBodyLimit, Json},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use prism_parsers::ParserRegistry;
use prism_render::html::HtmlRenderer;
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tracing::{info, Level};

use config::ServerConfig;

/// Application state
#[derive(Clone)]
struct AppState {
    /// Parser registry for format support
    parser_registry: Arc<ParserRegistry>,
    /// HTML renderer
    html_renderer: Arc<HtmlRenderer>,
    /// Server configuration
    config: Arc<ServerConfig>,
}

impl AppState {
    /// Create a new AppState with default configuration
    fn new() -> Self {
        let mut registry = ParserRegistry::new();

        // Register PDF parser
        registry.register(Arc::new(prism_parsers::PdfParser::new()));

        // Register image parsers
        registry.register(Arc::new(prism_parsers::PngParser::new()));
        registry.register(Arc::new(prism_parsers::JpegParser::new()));
        registry.register(Arc::new(prism_parsers::TiffParser::new()));

        // Register Office parsers (modern)
        registry.register(Arc::new(prism_parsers::DocxParser::new()));
        registry.register(Arc::new(prism_parsers::PptxParser::new()));
        registry.register(Arc::new(prism_parsers::XlsxParser::new()));

        // Register Office parsers (legacy)
        registry.register(Arc::new(prism_parsers::DocParser::new()));
        registry.register(Arc::new(prism_parsers::PptParser::new()));
        registry.register(Arc::new(prism_parsers::XlsParser::new()));

        // Register text-based parsers
        registry.register(Arc::new(prism_parsers::TextParser::new()));
        registry.register(Arc::new(prism_parsers::HtmlParser::new()));
        registry.register(Arc::new(prism_parsers::JsonParser::new()));
        registry.register(Arc::new(prism_parsers::XmlParser::new()));
        registry.register(Arc::new(prism_parsers::CsvParser::new()));
        registry.register(Arc::new(prism_parsers::MarkdownParser::new()));
        registry.register(Arc::new(prism_parsers::LogParser::new()));

        info!("Registered {} parsers", registry.count());

        // Log registered MIME types for debugging
        for parser in registry.all_parsers() {
            info!("  - {}: {}", parser.metadata().name, parser.format().mime_type);
        }

        let renderer = HtmlRenderer::new();
        let config = ServerConfig::default();

        Self {
            parser_registry: Arc::new(registry),
            html_renderer: Arc::new(renderer),
            config: Arc::new(config),
        }
    }
}

/// Health check response
#[derive(Debug, Serialize)]
struct HealthResponse {
    status: String,
    version: String,
}

/// Error response
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub message: String,
}

/// API error type
#[derive(Debug)]
pub enum ApiError {
    /// Bad request (400)
    BadRequest(String),
    /// Unsupported media type (415)
    UnsupportedMediaType(String),
    /// Not implemented (501)
    NotImplemented(String),
    /// Internal server error (500)
    InternalServerError(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            ApiError::UnsupportedMediaType(msg) => (StatusCode::UNSUPPORTED_MEDIA_TYPE, msg),
            ApiError::NotImplemented(msg) => (StatusCode::NOT_IMPLEMENTED, msg),
            ApiError::InternalServerError(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
        };

        let body = Json(ErrorResponse {
            error: status.to_string(),
            message,
        });

        (status, body).into_response()
    }
}

/// Health check endpoint
async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// Version endpoint
async fn version() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "server": env!("CARGO_PKG_VERSION"),
        "core": prism_core::VERSION,
        "parsers": prism_parsers::VERSION,
        "render": prism_render::VERSION,
        "sandbox": prism_sandbox::VERSION,
    }))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_target(false)
        .init();

    info!("Starting Prism Server v{}", env!("CARGO_PKG_VERSION"));

    // Initialize app state
    let state = AppState::new();

    // Build router with API routes
    let api_router = Router::new()
        .route("/health", get(health))
        .route("/version", get(version))
        .route("/convert", post(convert::convert))
        .layer(DefaultBodyLimit::max(5 * 1024 * 1024 * 1024)) // 5GB limit
        .with_state(state);

    // Combine API routes with static file serving
    let app = Router::new()
        .nest("/api", api_router)
        .nest_service("/", ServeDir::new("web-viewer"))
        .layer(CorsLayer::permissive());

    // Start server
    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    info!("Server listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

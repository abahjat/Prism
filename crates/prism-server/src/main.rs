//! # Prism Server
//!
//! REST API server for Prism document processing.
//!
//! This is the main entry point for the Prism HTTP server.

use axum::{
    extract::Json,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tower_http::cors::CorsLayer;
use tracing::{info, Level};

/// Application state
#[derive(Clone)]
struct AppState {
    // TODO: Add parser registry, configuration, etc.
}

/// Health check response
#[derive(Debug, Serialize)]
struct HealthResponse {
    status: String,
    version: String,
}

/// Error response
#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
    message: String,
}

/// API error type
#[derive(Debug)]
enum ApiError {
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
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
    let state = AppState {};

    // Build router
    let app = Router::new()
        .route("/health", get(health))
        .route("/version", get(version))
        // TODO: Add document processing routes
        .layer(CorsLayer::permissive())
        .with_state(state);

    // Start server
    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    info!("Server listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

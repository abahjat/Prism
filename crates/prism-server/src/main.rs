// SPDX-License-Identifier: AGPL-3.0-only
//! # Prism Server Binary Entry Point
//!
//! Starts the hardened Prism HTTP API server bound to `127.0.0.1:8080` by default.

use clap::Parser;
use prism_server::{AppState, ServerArgs, ServerConfig, build_app_router};
use std::net::SocketAddr;
use tracing::{Level, info};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = ServerArgs::parse();

    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_target(false)
        .init();

    info!(
        "Starting Prism Server v{} (engine: {})",
        prism_server::VERSION,
        prism_core::ENGINE_VERSION
    );

    let config = ServerConfig::from_args(&args);
    let state = AppState::with_config(config);
    let app = build_app_router(state, &args.cors_origins);

    let addr = SocketAddr::from((args.host, args.port));
    info!("Server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

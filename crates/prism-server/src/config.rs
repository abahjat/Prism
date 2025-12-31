// SPDX-License-Identifier: AGPL-3.0-only
//! Server configuration

use clap::Parser;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;

/// Command-line arguments for Prism Server
#[derive(Parser, Debug)]
#[command(name = "prism-server")]
#[command(about = "REST API server for Prism document processing")]
#[command(version)]
pub struct ServerArgs {
    /// Host address to bind to
    #[arg(short = 'H', long, default_value = "127.0.0.1", env = "PRISM_HOST")]
    pub host: IpAddr,

    /// Port to listen on
    #[arg(short, long, default_value = "8080", env = "PRISM_PORT")]
    pub port: u16,
}

/// Server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Maximum file size in bytes (default: 5GB)
    pub max_file_size: usize,

    /// Request timeout in seconds (default: 300s / 5 minutes)
    pub timeout_seconds: u64,

    /// Whether to enable fallback mode for unsupported formats
    pub enable_fallback: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_file_size: 5 * 1024 * 1024 * 1024, // 5GB
            timeout_seconds: 300,                  // 5 minutes for large files
            enable_fallback: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ServerConfig::default();
        assert_eq!(config.max_file_size, 5 * 1024 * 1024 * 1024);
        assert_eq!(config.timeout_seconds, 300);
        assert!(config.enable_fallback);
    }
}

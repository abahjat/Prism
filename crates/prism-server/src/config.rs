// SPDX-License-Identifier: AGPL-3.0-only
//! Server configuration and command-line arguments.

use clap::Parser;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::PathBuf;

/// Command-line arguments for Prism Server.
#[derive(Parser, Debug, Clone)]
#[command(name = "prism-server")]
#[command(about = "Hardened REST API server for Prism document processing")]
#[command(version)]
pub struct ServerArgs {
    /// Host address to bind to (defaults to localhost `127.0.0.1` for security).
    #[arg(short = 'H', long, default_value = "127.0.0.1", env = "PRISM_HOST")]
    pub host: IpAddr,

    /// Port to listen on.
    #[arg(short, long, default_value_t = 8080, env = "PRISM_PORT")]
    pub port: u16,

    /// Allowed CORS origins (comma-separated). If empty, defaults to documain.ai and localhost.
    /// Use `"*"` for permissive mode (development only).
    #[arg(long, env = "PRISM_CORS_ORIGINS", default_value = "")]
    pub cors_origins: String,

    /// Maximum request body / file size in MiB (default: `64`).
    #[arg(long, env = "PRISM_MAX_FILE_SIZE_MB", default_value_t = 64)]
    pub max_file_size_mb: usize,

    /// Per-request worker timeout in seconds (default: `60`).
    #[arg(long, env = "PRISM_TIMEOUT_SECS", default_value_t = 60)]
    pub timeout_seconds: u64,

    /// Per-job worker memory limit in MiB (default: `512`).
    #[arg(long, env = "PRISM_MAX_MEMORY_MB", default_value_t = 512)]
    pub max_memory_mb: usize,

    /// Maximum concurrent document-processing requests (default: `16`).
    #[arg(long, env = "PRISM_MAX_CONCURRENCY", default_value_t = 16)]
    pub max_concurrent_requests: usize,

    /// Worker pool size (default: `4`).
    #[arg(long, env = "PRISM_WORKER_POOL_SIZE", default_value_t = 4)]
    pub worker_pool_size: usize,

    /// Optional directory for the content-addressed artifact cache (`FilesystemCache`).
    #[arg(long, env = "PRISM_CACHE_DIR")]
    pub cache_dir: Option<PathBuf>,

    /// Maximum cache size in MiB when `--cache-dir` is enabled (default: `512`).
    #[arg(long, env = "PRISM_CACHE_MAX_MB", default_value_t = 512)]
    pub cache_max_mb: u64,

    /// Run document jobs in-process instead of spawning `prism-worker` subprocesses.
    #[arg(long, env = "PRISM_IN_PROCESS")]
    pub in_process: bool,
}

/// Runtime server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Maximum file size in bytes (default: `64 MiB`).
    pub max_file_size: usize,
    /// Per-request worker timeout in seconds (default: `60`).
    pub timeout_seconds: u64,
    /// Per-job worker memory limit in bytes (default: `512 MiB`).
    pub max_memory_bytes: usize,
    /// Maximum concurrent document-processing requests (default: `16`).
    pub max_concurrent_requests: usize,
    /// Worker pool size (default: `4`).
    pub worker_pool_size: usize,
    /// Optional content-addressed cache directory.
    pub cache_dir: Option<PathBuf>,
    /// Maximum cache size in bytes when `cache_dir` is set.
    pub cache_max_bytes: u64,
    /// Whether to run jobs in-process instead of spawning `prism-worker` subprocesses.
    pub in_process: bool,
    /// Whether to enable fallback format-detection preview for unsupported formats on `/api/convert`.
    pub enable_fallback: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_file_size: 64 * 1024 * 1024,
            timeout_seconds: 60,
            max_memory_bytes: 512 * 1024 * 1024,
            max_concurrent_requests: 16,
            worker_pool_size: 4,
            cache_dir: None,
            cache_max_bytes: 512 * 1024 * 1024,
            in_process: false,
            enable_fallback: true,
        }
    }
}

impl ServerConfig {
    /// Build a [`ServerConfig`] from parsed [`ServerArgs`].
    #[must_use]
    pub fn from_args(args: &ServerArgs) -> Self {
        Self {
            max_file_size: args.max_file_size_mb.saturating_mul(1024 * 1024),
            timeout_seconds: args.timeout_seconds.max(1),
            max_memory_bytes: args.max_memory_mb.saturating_mul(1024 * 1024),
            max_concurrent_requests: args.max_concurrent_requests.max(1),
            worker_pool_size: args.worker_pool_size.max(1),
            cache_dir: args.cache_dir.clone(),
            cache_max_bytes: args.cache_max_mb.saturating_mul(1024 * 1024),
            in_process: args.in_process,
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
        assert_eq!(config.max_file_size, 64 * 1024 * 1024);
        assert_eq!(config.timeout_seconds, 60);
        assert_eq!(config.max_concurrent_requests, 16);
        assert!(config.enable_fallback);
    }
}

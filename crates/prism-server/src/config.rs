//! Server configuration

use serde::{Deserialize, Serialize};

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
            timeout_seconds: 300, // 5 minutes for large files
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
        assert_eq!(config.max_file_size, 50 * 1024 * 1024);
        assert_eq!(config.timeout_seconds, 30);
        assert!(config.enable_fallback);
    }
}

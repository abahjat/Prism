//! # Prism Sandbox
//!
//! WebAssembly-based sandboxing for secure parser execution.
//!
//! This crate provides isolation for document parsers using WebAssembly,
//! ensuring that potentially malicious documents cannot compromise the system.
//!
//! ## Security Features
//!
//! - **Memory Limits**: Configurable memory limits per parser instance
//! - **CPU Limits**: Execution time limits and instruction counting
//! - **No I/O**: Sandboxed parsers cannot access filesystem or network
//! - **Deterministic**: Same input always produces same output
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────┐
//! │         Host Application             │
//! │                                      │
//! │  ┌────────────────────────────────┐  │
//! │  │     Sandbox Manager            │  │
//! │  │                                │  │
//! │  │  ┌──────────┐  ┌──────────┐   │  │
//! │  │  │ Parser 1 │  │ Parser 2 │   │  │
//! │  │  │  (WASM)  │  │  (WASM)  │   │  │
//! │  │  │          │  │          │   │  │
//! │  │  │ Memory:  │  │ Memory:  │   │  │
//! │  │  │  64 MB   │  │  64 MB   │   │  │
//! │  │  │ Timeout: │  │ Timeout: │   │  │
//! │  │  │  30s     │  │  30s     │   │  │
//! │  │  └──────────┘  └──────────┘   │  │
//! │  └────────────────────────────────┘  │
//! └─────────────────────────────────────┘
//! ```
//!
//! ## Usage
//!
//! ```rust,no_run
//! use prism_sandbox::{SandboxConfig, SandboxManager};
//!
//! # fn example() -> prism_core::Result<()> {
//! let config = SandboxConfig {
//!     max_memory: 64 * 1024 * 1024, // 64 MB
//!     max_execution_time: std::time::Duration::from_secs(30),
//!     max_instructions: Some(1_000_000_000),
//! };
//!
//! let manager = SandboxManager::new(config);
//!
//! // Load and execute WASM parser
//! // let result = manager.execute_parser(wasm_bytes, input_data)?;
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]
#![warn(clippy::all)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

use std::time::Duration;

/// Sandbox configuration
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Maximum memory per sandbox instance (in bytes)
    pub max_memory: usize,

    /// Maximum execution time
    pub max_execution_time: Duration,

    /// Maximum number of WASM instructions (optional)
    pub max_instructions: Option<u64>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            max_memory: 64 * 1024 * 1024, // 64 MB
            max_execution_time: Duration::from_secs(30),
            max_instructions: Some(1_000_000_000), // 1 billion instructions
        }
    }
}

/// Sandbox manager for executing parsers in isolation
#[derive(Debug)]
pub struct SandboxManager {
    config: SandboxConfig,
}

impl SandboxManager {
    /// Create a new sandbox manager with the given configuration
    #[must_use]
    pub fn new(config: SandboxConfig) -> Self {
        Self { config }
    }

    /// Create a new sandbox manager with default configuration
    #[must_use]
    pub fn default_config() -> Self {
        Self::new(SandboxConfig::default())
    }

    /// Get the sandbox configuration
    #[must_use]
    pub fn config(&self) -> &SandboxConfig {
        &self.config
    }
}

/// Prism sandbox version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandbox_config_default() {
        let config = SandboxConfig::default();
        assert_eq!(config.max_memory, 64 * 1024 * 1024);
        assert_eq!(config.max_execution_time, Duration::from_secs(30));
    }

    #[test]
    fn test_sandbox_manager_creation() {
        let manager = SandboxManager::default_config();
        assert_eq!(manager.config().max_memory, 64 * 1024 * 1024);
    }
}

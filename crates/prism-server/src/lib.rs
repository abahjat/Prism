// SPDX-License-Identifier: AGPL-3.0-only
//! # Prism Server Library
//!
//! Core library for the Prism REST API server.

#![warn(missing_docs)]
#![warn(clippy::all)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

/// Prism server version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// SPDX-License-Identifier: AGPL-3.0-only
//! CAD format parsers
//!
//! Supports:
//! - DXF (AutoCAD Drawing Exchange Format)

pub mod dxf;

pub use dxf::DxfParser;

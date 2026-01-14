// SPDX-License-Identifier: AGPL-3.0-only
//! Database parsers

/// dBase DBF parser
pub mod dbf;
/// `SQLite` database parser
pub mod sqlite;

pub use dbf::DbfParser;
pub use sqlite::SqliteParser;

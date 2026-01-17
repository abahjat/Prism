# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0] - 2026-01-17
### Changed
- Upgraded codebase to Rust Edition 2024 (requires Rust 1.85+).
- Updated `prism-bindings` to use `#[unsafe(no_mangle)]` per new edition safety requirements.
- Consolidated versioning across all crates (`prism-tests`, `prism-bindings`) to inherit from workspace root.
- Fixed clippy issues in `prism-parsers`.

## [0.1.0] - 2026-01-17
### Added
- Initial release of Prism.
- Core document model and trait definitions.
- Parser implementations for Archive, Office, and Email formats.
- Rust bindings crate `prism-bindings`.
- NuGet package `Prism.Native` including native runtimes for Windows (x64).

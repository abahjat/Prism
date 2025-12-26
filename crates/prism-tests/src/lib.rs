// SPDX-License-Identifier: AGPL-3.0-only
// Library entry point for integration tests
// Most logic will be in the tests/ directory
pub fn setup_test_logging() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();
}

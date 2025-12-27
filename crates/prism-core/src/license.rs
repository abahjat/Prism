// SPDX-License-Identifier: AGPL-3.0-only
//! # License Module
//!
//! Commercial license management and validation.

/// Commercial License Manager
///
/// Handles validation of commercial license keys.
/// In the AGPL version, this is a stub. In the commercial build,
/// this would contain actual validation logic (e.g., verifying signature).
pub struct LicenseManager;

impl LicenseManager {
    /// Validate a license key.
    ///
    /// # Arguments
    /// * `key` - The license string to validate.
    ///
    /// # Returns
    /// * `true` if valid, `false` otherwise.
    #[must_use]
    pub fn validate(key: &str) -> bool {
        // Placeholder validation logic
        // In a real scenario, this might check a JWT signature or online endpoint.
        // For the open source version, we might just return false or a default "community" state.

        if key == "commercial-dev-key-123" {
            return true;
        }

        false
    }

    /// Get the current license type.
    #[must_use]
    pub fn license_type() -> &'static str {
        "AGPL-3.0 (Community)"
    }
}

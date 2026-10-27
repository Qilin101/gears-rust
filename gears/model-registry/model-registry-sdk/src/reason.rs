// Created: 2026-10-26 by Constructor Tech
//! Reason codes carried in `CanonicalError` contexts (DESIGN §4 Error
//! Handling).

/// `PermissionDenied` reasons (`context.reason`).
pub mod permission {
    /// The model's provider is disabled (`resolve_model`).
    pub const PROVIDER_DISABLED: &str = "PROVIDER_DISABLED";
    /// The model is pending, rejected, or revoked (`resolve_model`).
    pub const MODEL_NOT_APPROVED: &str = "MODEL_NOT_APPROVED";
    /// The PDP denied the operation.
    pub const NOT_PERMITTED: &str = "NOT_PERMITTED";
    /// A write addressed an inherited provider or model.
    pub const INHERITED_READ_ONLY: &str = "INHERITED_READ_ONLY";
}

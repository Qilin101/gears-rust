// Created: 2026-05-06 by Constructor Tech
//! Transport-agnostic request DTOs for the Model Registry SDK.
//!
//! These are NOT REST DTOs — they sit on the SDK trait (`ModelRegistryClientV1`)
//! and are serialized into transport (REST/gRPC) by the module crate.

use crate::models::{
    ApprovalStatus, ContextWindow, DefaultInferenceParametersV1, DisabledCapabilities,
    LifecycleStatus, ModelCapabilities, ModelInfoV1, ModelPerformance, ProviderStatus,
};

// ---------------------------------------------------------------------------
// CreateProviderRequestV1 (builder pattern)
// ---------------------------------------------------------------------------

/// Request for registering a new provider. Construct via
/// [`CreateProviderRequestV1::builder`].
#[derive(Debug, Clone, PartialEq)]
pub struct CreateProviderRequestV1 {
    slug: String,
    name: String,
    gts_type: gts::GtsTypeId,
    managed: bool,
    metadata: Option<serde_json::Value>,
    discovery_enabled: bool,
    discovery_interval_seconds: Option<u32>,
}

impl CreateProviderRequestV1 {
    /// Start building a new request. All three fields are required.
    #[must_use]
    pub fn builder(
        slug: impl Into<String>,
        name: impl Into<String>,
        gts_type: gts::GtsTypeId,
    ) -> CreateProviderRequestV1Builder {
        CreateProviderRequestV1Builder {
            slug: slug.into(),
            name: name.into(),
            gts_type,
            managed: false,
            metadata: None,
            discovery_enabled: false,
            discovery_interval_seconds: None,
        }
    }

    #[must_use]
    pub fn slug(&self) -> &str {
        &self.slug
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn gts_type(&self) -> &gts::GtsTypeId {
        &self.gts_type
    }

    #[must_use]
    pub fn managed(&self) -> bool {
        self.managed
    }

    #[must_use]
    pub fn metadata(&self) -> Option<&serde_json::Value> {
        self.metadata.as_ref()
    }

    #[must_use]
    pub fn discovery_enabled(&self) -> bool {
        self.discovery_enabled
    }

    #[must_use]
    pub fn discovery_interval_seconds(&self) -> Option<u32> {
        self.discovery_interval_seconds
    }
}

#[derive(Debug, Clone)]
pub struct CreateProviderRequestV1Builder {
    slug: String,
    name: String,
    gts_type: gts::GtsTypeId,
    managed: bool,
    metadata: Option<serde_json::Value>,
    discovery_enabled: bool,
    discovery_interval_seconds: Option<u32>,
}

impl CreateProviderRequestV1Builder {
    #[must_use]
    pub fn managed(mut self, managed: bool) -> Self {
        self.managed = managed;
        self
    }

    #[must_use]
    pub fn metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    #[must_use]
    pub fn discovery_enabled(mut self, enabled: bool) -> Self {
        self.discovery_enabled = enabled;
        self
    }

    #[must_use]
    pub fn discovery_interval_seconds(mut self, seconds: u32) -> Self {
        self.discovery_interval_seconds = Some(seconds);
        self
    }

    #[must_use]
    pub fn build(self) -> CreateProviderRequestV1 {
        CreateProviderRequestV1 {
            slug: self.slug,
            name: self.name,
            gts_type: self.gts_type,
            managed: self.managed,
            metadata: self.metadata,
            discovery_enabled: self.discovery_enabled,
            discovery_interval_seconds: self.discovery_interval_seconds,
        }
    }
}

// ---------------------------------------------------------------------------
// UpdateProviderRequestV1 (PATCH semantics)
// ---------------------------------------------------------------------------

/// Request for updating a provider (PATCH semantics). Only non-`None` fields
/// are applied.
///
/// Nullable columns use tri-state `Option<Option<T>>` to distinguish "field
/// omitted — leave unchanged" (`None`) from "explicitly clear to null"
/// (`Some(None)`) and "set to a value" (`Some(Some(v))`). Non-nullable columns
/// stay `Option<T>` (`None` = unchanged, `Some(v)` = set).
#[derive(Debug, Clone, Default, PartialEq)]
#[allow(clippy::option_option)]
pub struct UpdateProviderRequestV1 {
    pub name: Option<String>,
    pub status: Option<ProviderStatus>,
    pub managed: Option<bool>,
    /// Nullable — `Some(None)` clears stored metadata.
    pub metadata: Option<Option<serde_json::Value>>,
    pub discovery_enabled: Option<bool>,
    /// Nullable — `Some(None)` clears the discovery interval.
    pub discovery_interval_seconds: Option<Option<u32>>,
}

// ---------------------------------------------------------------------------
// CreateModelRequestV1 (P1 — manual model management)
// ---------------------------------------------------------------------------

/// Request for manually creating a model in the catalog (P1 manual model
/// management; `cpt-cf-model-registry-fr-manual-model-management`).
///
/// The `canonical_id` is derived from `provider_slug` + `info.provider_model_id`
/// — both are immutable after creation. Provider must exist for the caller's
/// tenant (or be inherited from an ancestor).
///
/// The optional `approval_status` defaults to [`ApprovalStatus::Pending`];
/// admins can pass [`ApprovalStatus::Approved`] to approve in the same call
/// as a convenience.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateModelRequestV1 {
    /// Provider slug (1-64 chars, lowercase alphanumeric + hyphen). Combined
    /// with `info.provider_model_id` to form the `canonical_id`.
    pub provider_slug: String,
    /// Lifecycle status (Production / Preview / Experimental / …).
    pub lifecycle_status: LifecycleStatus,
    /// Optional initial approval status. `None` ⇒ defaults to
    /// [`ApprovalStatus::Pending`].
    pub approval_status: Option<ApprovalStatus>,
    /// Model info — display, capabilities, limits, default parameters, and
    /// the provider-specific settings payload (raw JSON typed by
    /// `info.gts_type`).
    pub info: ModelInfoV1<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// UpdateModelRequestV1 (P1 — manual model management; PATCH semantics)
// ---------------------------------------------------------------------------

/// Request for updating an existing model. Only non-`None` fields are applied.
///
/// **Immutable after creation** — these fields are NOT in this struct:
/// `canonical_id`, `provider_slug`, `info.provider_model_id`, `info.gts_type`.
/// To switch a model's provider settings shape, soft-delete and recreate.
///
/// **Approval status changes** also flow through this PATCH endpoint (see
/// `cpt-cf-model-registry-fr-manual-model-management` in DESIGN §1.2):
/// setting `approval_status` updates `models.approval_status`.
///
/// Nullable columns use tri-state `Option<Option<T>>` to distinguish "field
/// omitted — leave unchanged" (`None`) from "explicitly clear to null"
/// (`Some(None)`) and "set to a value" (`Some(Some(v))`). Non-nullable columns
/// and wholesale-replacement fields stay `Option<T>` (`None` = unchanged,
/// `Some(v)` = set/replace).
#[derive(Debug, Clone, Default, PartialEq)]
#[allow(clippy::option_option)]
pub struct UpdateModelRequestV1 {
    // ── Status ────────────────────────────────────────────────────────
    /// Approval status (`approved` / `rejected` / `revoked` / `pending`).
    pub approval_status: Option<ApprovalStatus>,
    /// Lifecycle status (e.g. promote `Experimental` → `Production`, or mark
    /// `Sunset`). Setting to `Deprecated` here is equivalent to the soft-delete
    /// path; prefer [`crate::api::ModelRegistryClientV1::delete_model`].
    pub lifecycle_status: Option<LifecycleStatus>,

    // ── Display / discovery ───────────────────────────────────────────
    /// Non-nullable — `Some(v)` sets the display name.
    pub display_name: Option<String>,
    /// Nullable — `Some(None)` clears the description.
    pub description: Option<Option<String>>,
    /// Nullable — `Some(None)` clears the family label.
    pub family: Option<Option<String>>,
    /// Nullable — `Some(None)` clears the vendor label.
    pub vendor: Option<Option<String>>,
    /// Per-model infrastructure flag for local/managed LLMs (distinct from the
    /// per-provider `managed` flag). Non-nullable — `None` leaves it unchanged.
    pub managed: Option<bool>,
    /// Infrastructure field (for local/managed LLMs): model architecture
    /// classifier (e.g. `"qwen"`, `"llama"`). Nullable — `Some(None)` clears it.
    pub architecture: Option<Option<String>>,
    /// Infrastructure field (for local/managed LLMs): on-disk model size in
    /// bytes. Nullable — `Some(None)` clears it.
    pub size_bytes: Option<Option<u64>>,
    /// Infrastructure field (for local/managed LLMs): model weight/serving
    /// format (e.g. `"gguf"`, `"safetensors"`). Nullable — `Some(None)` clears it.
    pub format: Option<Option<String>>,
    /// Nullable — `Some(None)` clears the region.
    pub region: Option<Option<String>>,
    /// Nullable — `Some(None)` clears the host label.
    pub hosted_by: Option<Option<String>>,
    /// Nullable — `Some(None)` clears the reasoning-level label.
    pub reasoning_level: Option<Option<String>>,
    /// Nullable — `Some(None)` clears the version string.
    pub version: Option<Option<String>>,
    /// Nullable — `Some(None)` clears the sort order.
    pub sort_order: Option<Option<i32>>,
    /// Nullable — `Some(None)` clears the icon URL.
    pub icon: Option<Option<String>>,
    /// Nullable — `Some(None)` clears the multiplier label.
    pub multiplier_display: Option<Option<String>>,
    pub performance: Option<ModelPerformance>,

    // ── Capabilities & limits (full replacement) ──────────────────────
    /// Replace `info.capabilities` wholesale.
    pub capabilities: Option<ModelCapabilities>,
    /// Replace `info.disabled_capabilities` wholesale.
    pub disabled_capabilities: Option<DisabledCapabilities>,
    /// Replace `info.context_window` wholesale.
    pub context_window: Option<ContextWindow>,

    // ── Defaults & override policy ────────────────────────────────────
    /// Replace `info.default_parameters` wholesale.
    pub default_parameters: Option<DefaultInferenceParametersV1>,
    pub allow_parameter_override: Option<bool>,
    pub allow_extra_params: Option<Vec<String>>,

    // ── Provider-specific payload ─────────────────────────────────────
    /// Replace `info.provider_settings` wholesale. The shape MUST validate
    /// against the model's existing `info.gts_type` (which is immutable).
    pub provider_settings: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

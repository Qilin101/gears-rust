//! Model Registry Gear Implementation
//!
//! The public API is defined in `model-registry-sdk` and re-exported here.

pub use model_registry_sdk::{
    ApprovalStatus, CreateModelRequestV1, CreateProviderRequestV1, LifecycleStatus,
    ModelRegistryClientV1, ModelRegistryError, ModelV1, ProviderStatus, ProviderV1,
    UpdateModelRequestV1, UpdateProviderRequestV1,
};

pub mod gear;
pub use gear::ModelRegistryGear;

#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod infra;

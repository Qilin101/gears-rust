use chrono::Utc;
use model_registry_sdk::models::{
    CreateProviderRequestV1, ProviderStatus, UpdateProviderRequestV1,
};
use serde_json::json;
use uuid::Uuid;

use crate::domain::error::DomainError;

use super::super::entity;

use super::{provider_create_active_model, provider_entity_to_v1, provider_update_active_model};

// ---------------------------------------------------------------------------
// Test helpers — construct domain types via struct literals.
// ---------------------------------------------------------------------------

fn test_tenant_id() -> Uuid {
    Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()
}

fn test_provider_id() -> Uuid {
    Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap()
}

fn make_provider_entity(
    id: Uuid,
    tenant_id: Uuid,
    slug: &str,
    status: &str,
) -> entity::provider::Model {
    entity::provider::Model {
        id,
        tenant_id,
        slug: slug.to_owned(),
        name: format!("Provider {slug}"),
        gts_type: "gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~".to_owned(),
        status: status.to_owned(),
        managed: false,
        metadata: Some(json!({"region": "us-east"})),
        discovery_enabled: false,
        discovery_interval_seconds: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Provider entity → SDK
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn provider_entity_to_v1_active() {
    let entity = make_provider_entity(test_provider_id(), test_tenant_id(), "openai", "active");
    let v1 = provider_entity_to_v1(entity).expect("provider maps");

    assert_eq!(v1.id, test_provider_id());
    assert_eq!(v1.slug, "openai");
    assert_eq!(v1.status, ProviderStatus::Active);
    assert_eq!(
        v1.gts_type.as_ref(),
        "gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~"
    );
    assert!(v1.metadata.is_some());
}

#[test]
fn provider_entity_to_v1_disabled() {
    let entity = make_provider_entity(test_provider_id(), test_tenant_id(), "old", "disabled");
    let v1 = provider_entity_to_v1(entity).expect("provider maps");

    assert_eq!(v1.status, ProviderStatus::Disabled);
}

#[test]
fn provider_entity_to_v1_no_metadata() {
    let mut entity =
        make_provider_entity(test_provider_id(), test_tenant_id(), "no-meta", "active");
    entity.metadata = None;
    let v1 = provider_entity_to_v1(entity).expect("provider maps");

    assert!(v1.metadata.is_none());
}

#[test]
fn provider_entity_to_v1_rejects_corrupt_status() {
    // A `status` string outside the ProviderStatus domain must surface as a
    // typed Internal error, not a panic.
    let mut entity = make_provider_entity(test_provider_id(), test_tenant_id(), "openai", "active");
    entity.status = "retired".to_owned();

    let err = provider_entity_to_v1(entity).expect_err("out-of-domain status must be rejected");
    assert!(
        matches!(&err, DomainError::Internal { .. }),
        "expected Internal, got {err:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Provider create ActiveModel
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn provider_create_sets_fields() {
    let gts = gts::GtsTypeId::new("gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~");
    let req = CreateProviderRequestV1::builder("openai", "OpenAI", gts)
        .managed(true)
        .metadata(json!({"k": "v"}))
        .discovery_enabled(true)
        .discovery_interval_seconds(3600)
        .build();

    let am = provider_create_active_model(test_tenant_id(), &req);

    assert_eq!(am.tenant_id.unwrap(), test_tenant_id());
    assert_eq!(am.slug.unwrap(), "openai");
    assert_eq!(am.name.unwrap(), "OpenAI");
    assert_eq!(am.status.unwrap(), "active");
    assert!(am.managed.unwrap());
    assert!(am.discovery_enabled.unwrap());
    assert_eq!(am.discovery_interval_seconds.unwrap(), Some(3600));
}

#[test]
fn provider_create_defaults() {
    let gts = gts::GtsTypeId::new("gts.cf.genai.model.provider.v1~cf.genai._.custom.v1~");
    let req = CreateProviderRequestV1::builder("custom", "Custom", gts).build();

    let am = provider_create_active_model(test_tenant_id(), &req);

    assert_eq!(am.status.unwrap(), "active");
    assert!(!am.managed.unwrap());
    assert!(am.metadata.unwrap().is_none());
    assert!(!am.discovery_enabled.unwrap());
}

// ═══════════════════════════════════════════════════════════════════════════════
// Provider update ActiveModel
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn provider_update_only_some_fields() {
    let entity = make_provider_entity(test_provider_id(), test_tenant_id(), "openai", "active");
    let req = UpdateProviderRequestV1 {
        name: Some("Updated".into()),
        status: Some(ProviderStatus::Disabled),
        managed: None,
        metadata: None,
        discovery_enabled: None,
        discovery_interval_seconds: None,
    };

    let am = provider_update_active_model(&entity, &req);

    assert_eq!(am.name.unwrap(), "Updated");
    assert_eq!(am.status.unwrap(), "disabled");
    // Unchanged fields preserved
    assert!(!am.managed.unwrap());
}

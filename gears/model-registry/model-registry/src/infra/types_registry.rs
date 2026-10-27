//! [`SchemaSource`] over the Types Registry client (DESIGN §3.5 Provider
//! Settings Schemas), bounded by `types_registry_deadline_ms`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use types_registry_sdk::{TypesRegistryClient, TypesRegistryError};

use crate::domain::error::{Dependency, DomainError};
use crate::domain::gts_validator::SchemaSource;

pub struct TypesRegistrySchemaSource {
    client: Arc<dyn TypesRegistryClient>,
    deadline: Duration,
}

impl TypesRegistrySchemaSource {
    #[must_use]
    pub fn new(client: Arc<dyn TypesRegistryClient>, deadline: Duration) -> Self {
        Self { client, deadline }
    }
}

#[async_trait]
impl SchemaSource for TypesRegistrySchemaSource {
    async fn resolved_schema(
        &self,
        _ctx: &SecurityContext,
        type_id: &gts::GtsTypeId,
    ) -> Result<Option<serde_json::Value>, DomainError> {
        let call = self.client.get_type_schema(type_id.as_ref());
        let result = match tokio::time::timeout(self.deadline, call).await {
            Ok(result) => result,
            Err(_elapsed) => {
                return Err(DomainError::unavailable(
                    Dependency::TypesRegistry,
                    format!("get_type_schema did not answer within {:?}", self.deadline),
                ));
            }
        };
        match result {
            Ok(schema) => Ok(Some(schema.effective_schema())),
            Err(err) => match TypesRegistryError::from(err) {
                TypesRegistryError::NotFound { .. } | TypesRegistryError::Validation { .. } => {
                    Ok(None)
                }
                TypesRegistryError::Other { canonical }
                    if matches!(
                        canonical,
                        toolkit_canonical_errors::CanonicalError::PermissionDenied { .. }
                    ) =>
                {
                    Err(DomainError::internal(format!(
                        "types-registry refused the schema read of {type_id}: the gear's caller lacks the Types Registry read grant: {canonical}"
                    )))
                }
                other => Err(DomainError::unavailable(Dependency::TypesRegistry, other)),
            },
        }
    }
}

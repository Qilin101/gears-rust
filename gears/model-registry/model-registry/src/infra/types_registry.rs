//! [`SchemaSource`] over the Types Registry client (DESIGN §3.5 Provider
//! Settings Schemas), bounded by `types_registry_deadline_ms`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use types_registry_sdk::{TypesRegistryClient, TypesRegistryError};

use crate::domain::error::{Dependency, DomainError};
use crate::domain::gts_validator::SchemaSource;

/// Operation name reported when Types Registry denies the schema read.
pub const SCHEMA_READ_OPERATION: &str = "types_registry.get_type_schema";

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

/// Types Registry outcome mapping of DESIGN §4 Error Handling: not found is
/// `None`, a denied read is `NOT_PERMITTED`, everything else is dependency
/// unavailable.
fn map_result(
    result: Result<serde_json::Value, TypesRegistryError>,
    type_id: &gts::GtsTypeId,
) -> Result<Option<serde_json::Value>, DomainError> {
    match result {
        Ok(schema) => Ok(Some(schema)),
        Err(TypesRegistryError::NotFound { .. } | TypesRegistryError::Validation { .. }) => {
            Ok(None)
        }
        Err(TypesRegistryError::Other { canonical })
            if matches!(canonical, CanonicalError::PermissionDenied { .. }) =>
        {
            tracing::warn!(
                %type_id,
                error = %canonical,
                "types-registry refused the schema read: the caller lacks the Types Registry read grant"
            );
            Err(DomainError::NotPermitted {
                operation: SCHEMA_READ_OPERATION,
            })
        }
        Err(other) => Err(DomainError::unavailable(Dependency::TypesRegistry, other)),
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
        map_result(
            result
                .map(|schema| schema.effective_schema())
                .map_err(TypesRegistryError::from),
            type_id,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_id() -> gts::GtsTypeId {
        gts::GtsTypeId::new("gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~")
    }

    #[test]
    fn denied_read_is_not_permitted() {
        let err = TypesRegistryError::Other {
            canonical: crate::domain::error::ModelScope::permission_denied()
                .with_reason("NO_READ_GRANT")
                .create(),
        };
        assert!(matches!(
            map_result(Err(err), &type_id()),
            Err(DomainError::NotPermitted {
                operation: SCHEMA_READ_OPERATION
            })
        ));
    }

    #[test]
    fn not_found_is_none_and_failures_are_unavailable() {
        let not_found = TypesRegistryError::NotFound {
            resource_type: "type".into(),
            name: "x".into(),
            detail: String::new(),
        };
        assert!(matches!(map_result(Err(not_found), &type_id()), Ok(None)));

        let down = TypesRegistryError::Other {
            canonical: CanonicalError::service_unavailable().create(),
        };
        assert!(matches!(
            map_result(Err(down), &type_id()),
            Err(DomainError::DependencyUnavailable {
                dependency: Dependency::TypesRegistry,
                ..
            })
        ));
    }
}

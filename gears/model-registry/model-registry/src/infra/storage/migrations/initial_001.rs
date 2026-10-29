//! Initial schema (DESIGN §3.7): `providers` and `models` with their unique
//! keys, the composite cascading foreign key, the value checks, and one B-tree
//! index per filterable field. PostgreSQL and SQLite share the same shape.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Logical type names per backend.
struct Types {
    uuid: &'static str,
    timestamp: &'static str,
    json: &'static str,
    bigint: &'static str,
}

fn types_for(backend: sea_orm::DatabaseBackend) -> Result<Types, DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres => Ok(Types {
            uuid: "UUID",
            timestamp: "TIMESTAMPTZ",
            json: "JSONB",
            bigint: "BIGINT",
        }),
        sea_orm::DatabaseBackend::Sqlite => Ok(Types {
            uuid: "TEXT",
            timestamp: "TEXT",
            json: "TEXT",
            bigint: "INTEGER",
        }),
        other => Err(DbErr::Custom(format!(
            "model-registry supports PostgreSQL and SQLite, not {other:?}"
        ))),
    }
}

/// Filterable provider columns of DESIGN §3.3 that get a single-column index
/// (the unique `(tenant_id, slug)` key covers `tenant_id`, the PK covers `id`).
const PROVIDER_INDEXED: &[&str] = &[
    "slug",
    "name",
    "gts_type",
    "enabled",
    "sharing_mode",
    "managed",
    "created_at",
    "updated_at",
];

/// Filterable model columns of DESIGN §3.3 that get a single-column index.
const MODEL_INDEXED: &[&str] = &[
    "tenant_id",
    "provider_id",
    "canonical_id",
    "provider_model_id",
    "gts_type",
    "sharing_mode",
    "approval_state",
    "approval_decided_at",
    "cap_vision",
    "cap_reasoning_effort",
    "cap_tools",
    "cap_json_schema",
    "cap_streaming",
    "cap_file_input",
    "cap_image_generation",
    "cap_audio_input",
    "cap_audio_output",
    "cap_code_interpreter",
    "cap_web_search",
    "api_completion",
    "api_embedding",
    "api_batch",
    "display_name",
    "family",
    "vendor",
    "hosted_by",
    "region",
    "version",
    "managed",
    "architecture",
    "format",
    "sort_order",
    "last_release_at",
    "ctx_max_input_tokens",
    "ctx_max_output_tokens",
    "created_at",
    "updated_at",
];

fn index_statements(table: &str, columns: &[&str]) -> String {
    use std::fmt::Write as _;
    columns.iter().fold(String::new(), |mut sql, c| {
        // Writing to a `String` cannot fail.
        writeln!(
            sql,
            "CREATE INDEX IF NOT EXISTS idx_{table}_{c} ON {table} ({c});"
        )
        .ok();
        sql
    })
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let Types {
            uuid,
            timestamp,
            json,
            bigint,
        } = types_for(manager.get_database_backend())?;

        let providers = format!(
            r"
CREATE TABLE IF NOT EXISTS providers (
    id             {uuid} NOT NULL,
    tenant_id      {uuid} NOT NULL,
    slug           VARCHAR(64) NOT NULL,
    name           VARCHAR(255) NOT NULL,
    gts_type       VARCHAR(512) NOT NULL,
    enabled        BOOLEAN NOT NULL,
    sharing_mode   VARCHAR(16) NOT NULL,
    managed        BOOLEAN NOT NULL,
    metadata       {json},
    created_at     {timestamp} NOT NULL,
    created_by     {uuid} NOT NULL,
    updated_at     {timestamp} NOT NULL,
    updated_by     {uuid} NOT NULL,
    PRIMARY KEY (id),
    UNIQUE (tenant_id, slug),
    UNIQUE (id, tenant_id),
    CHECK (sharing_mode IN ('tenant', 'shared'))
);
CREATE INDEX IF NOT EXISTS idx_providers_slug_id ON providers (slug, id);
"
        );

        let models = format!(
            r"
CREATE TABLE IF NOT EXISTS models (
    id                        {uuid} NOT NULL,
    tenant_id                 {uuid} NOT NULL,
    provider_id               {uuid} NOT NULL,
    provider_model_id         VARCHAR(255) NOT NULL,
    canonical_id              VARCHAR(321) NOT NULL,
    gts_type                  VARCHAR(512) NOT NULL,
    sharing_mode              VARCHAR(16) NOT NULL,
    approval_state            VARCHAR(16) NOT NULL,
    approval_decided_by       {uuid} NOT NULL,
    approval_decided_at       {timestamp} NOT NULL,
    base_url                  VARCHAR(2048),
    secret_ref                VARCHAR(255),
    display_name              VARCHAR(255) NOT NULL,
    description               TEXT,
    family                    VARCHAR(128),
    vendor                    VARCHAR(128),
    managed                   BOOLEAN NOT NULL,
    architecture              VARCHAR(64),
    size_bytes                {bigint},
    format                    VARCHAR(64),
    region                    VARCHAR(64),
    hosted_by                 VARCHAR(128),
    last_release_at           {timestamp},
    reasoning_level           VARCHAR(64),
    version                   VARCHAR(64),
    sort_order                INTEGER NOT NULL DEFAULT 0,
    icon                      VARCHAR(2048),
    multiplier_display        VARCHAR(32),
    performance               {json} NOT NULL,
    additional_info           {json} NOT NULL,
    supported_api             {json} NOT NULL,
    api_completion            BOOLEAN NOT NULL,
    api_embedding             BOOLEAN NOT NULL,
    api_batch                 BOOLEAN NOT NULL,
    capabilities              {json} NOT NULL,
    disabled_capabilities     {json} NOT NULL,
    cap_vision                BOOLEAN NOT NULL,
    cap_reasoning_effort      BOOLEAN NOT NULL,
    cap_tools      BOOLEAN NOT NULL,
    cap_json_schema       BOOLEAN NOT NULL,
    cap_streaming             BOOLEAN NOT NULL,
    cap_file_input            BOOLEAN NOT NULL,
    cap_image_generation      BOOLEAN NOT NULL,
    cap_audio_input           BOOLEAN NOT NULL,
    cap_audio_output          BOOLEAN NOT NULL,
    cap_code_interpreter      BOOLEAN NOT NULL,
    cap_web_search            BOOLEAN NOT NULL,
    ctx_max_input_tokens      {bigint} NOT NULL,
    ctx_max_output_tokens     {bigint},
    ctx_output_vector_size    {bigint},
    default_parameters        {json} NOT NULL,
    allow_parameter_override  BOOLEAN NOT NULL,
    allow_extra_params        {json} NOT NULL,
    provider_settings         {json} NOT NULL,
    created_at                {timestamp} NOT NULL,
    created_by                {uuid} NOT NULL,
    updated_at                {timestamp} NOT NULL,
    updated_by                {uuid} NOT NULL,
    PRIMARY KEY (id),
    UNIQUE (tenant_id, canonical_id),
    UNIQUE (provider_id, provider_model_id),
    FOREIGN KEY (provider_id, tenant_id) REFERENCES providers (id, tenant_id) ON DELETE CASCADE,
    CHECK (sharing_mode IN ('tenant', 'shared')),
    CHECK (approval_state IN ('pending', 'approved', 'rejected', 'revoked'))
);
CREATE INDEX IF NOT EXISTS idx_models_canonical_id_id ON models (canonical_id, id);
"
        );

        let sql = format!(
            "{providers}{}{models}{}",
            index_statements("providers", PROVIDER_INDEXED),
            index_statements("models", MODEL_INDEXED)
        );
        for statement in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            conn.execute_unprepared(statement).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared("DROP TABLE IF EXISTS models")
            .await?;
        conn.execute_unprepared("DROP TABLE IF EXISTS providers")
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DbBackend, Statement};

    async fn migrated() -> sea_orm::DatabaseConnection {
        let conn = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite connection");
        Migration::up(&Migration, &SchemaManager::new(&conn))
            .await
            .unwrap();
        conn
    }

    async fn exec(conn: &sea_orm::DatabaseConnection, sql: &str) -> Result<(), DbErr> {
        conn.execute_unprepared(sql).await.map(|_| ())
    }

    const PROVIDER: &str = "INSERT INTO providers (id, tenant_id, slug, name, gts_type, enabled, sharing_mode, managed, metadata, created_at, created_by, updated_at, updated_by) VALUES ('p1', 't1', 'openai', 'OpenAI', 'gts.x~', 1, 'shared', 0, NULL, 'now', 's', 'now', 's')";

    fn model(id: &str, provider: &str, tenant: &str) -> String {
        format!(
            "INSERT INTO models (id, tenant_id, provider_id, provider_model_id, canonical_id, gts_type, sharing_mode, approval_state, approval_decided_by, approval_decided_at, display_name, managed, performance, additional_info, supported_api, api_completion, api_embedding, api_batch, capabilities, disabled_capabilities, cap_vision, cap_reasoning_effort, cap_tools, cap_json_schema, cap_streaming, cap_file_input, cap_image_generation, cap_audio_input, cap_audio_output, cap_code_interpreter, cap_web_search, ctx_max_input_tokens, default_parameters, allow_parameter_override, allow_extra_params, provider_settings, created_at, created_by, updated_at, updated_by) VALUES ('{id}', '{tenant}', '{provider}', 'gpt', 'openai::gpt', 'gts.x~', 'tenant', 'approved', 's', 'now', 'GPT', 0, '{{}}', '{{}}', '[]', 0, 0, 0, '{{}}', '{{}}', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, '{{}}', 0, '[]', '{{}}', 'now', 's', 'now', 's')"
        )
    }

    #[tokio::test]
    async fn up_down_roundtrip() {
        let conn = sea_orm::Database::connect("sqlite::memory:").await.unwrap();
        let manager = SchemaManager::new(&conn);
        assert!(!manager.has_table("providers").await.unwrap());
        Migration::up(&Migration, &manager).await.unwrap();
        assert!(manager.has_table("providers").await.unwrap());
        assert!(manager.has_table("models").await.unwrap());
        Migration::up(&Migration, &manager).await.unwrap();
        Migration::down(&Migration, &manager).await.unwrap();
        assert!(!manager.has_table("models").await.unwrap());
    }

    #[tokio::test]
    async fn foreign_key_is_enforced_and_cascades() {
        let conn = migrated().await;
        exec(&conn, PROVIDER).await.unwrap();
        let err = exec(&conn, &model("m0", "missing", "t1"))
            .await
            .unwrap_err();
        assert!(toolkit_db::secure::is_foreign_key_violation(&err), "{err}");
        let err = exec(&conn, &model("m0", "p1", "other-tenant"))
            .await
            .unwrap_err();
        assert!(toolkit_db::secure::is_foreign_key_violation(&err), "{err}");
        exec(&conn, &model("m1", "p1", "t1")).await.unwrap();
        exec(&conn, "DELETE FROM providers WHERE id = 'p1'")
            .await
            .unwrap();
        let rows = conn
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT id FROM models",
            ))
            .await
            .unwrap();
        assert!(rows.is_empty(), "models must cascade with their provider");
    }

    #[tokio::test]
    async fn unique_keys_and_checks_hold() {
        let conn = migrated().await;
        exec(&conn, PROVIDER).await.unwrap();
        let err = exec(&conn, &PROVIDER.replace("'p1'", "'p2'"))
            .await
            .unwrap_err();
        assert!(toolkit_db::secure::is_unique_violation(&err), "{err}");
        let err = exec(
            &conn,
            &PROVIDER
                .replace("'p1', 't1', 'openai'", "'p3', 't1', 'x'")
                .replace("'shared'", "'children'"),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("check"), "{err}");

        exec(&conn, &model("m1", "p1", "t1")).await.unwrap();
        let err = exec(&conn, &model("m2", "p1", "t1")).await.unwrap_err();
        assert!(toolkit_db::secure::is_unique_violation(&err), "{err}");
        let err = exec(
            &conn,
            &model("m3", "p1", "t1")
                .replace("'approved'", "'maybe'")
                .replace("'gpt', 'openai::gpt'", "'gpt2', 'openai::gpt2'"),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("check"), "{err}");
    }

    async fn index_columns(conn: &sea_orm::DatabaseConnection, table: &str) -> Vec<Vec<String>> {
        let idx_rows = conn
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                format!("SELECT name FROM pragma_index_list('{table}');"),
            ))
            .await
            .unwrap();
        let mut out = Vec::new();
        for row in &idx_rows {
            let Ok(idx) = row.try_get_by::<String, _>("name") else {
                continue;
            };
            let cols = conn
                .query_all_raw(Statement::from_string(
                    DbBackend::Sqlite,
                    format!("SELECT name FROM pragma_index_info('{idx}') ORDER BY seqno;"),
                ))
                .await
                .unwrap()
                .iter()
                .filter_map(|r| r.try_get_by::<String, _>("name").ok())
                .collect();
            out.push(cols);
        }
        out
    }

    /// Every filterable field has an index that starts with its column
    /// (DESIGN §3.3), derived from the filter surface itself.
    #[tokio::test]
    async fn every_filterable_column_leads_an_index() {
        use crate::infra::storage::odata_mapper::{ModelODataMapper, ProviderODataMapper};
        use model_registry_sdk::odata::{ModelFilterField, ProviderFilterField};
        use toolkit_db::odata::sea_orm_filter::FieldToColumn;
        use toolkit_odata::filter::FilterField;

        let conn = migrated().await;
        let leads = |indexes: &[Vec<String>], column: &str| {
            indexes
                .iter()
                .any(|cols| cols.first().is_some_and(|c| c == column))
        };

        let model_indexes = index_columns(&conn, "models").await;
        for field in ModelFilterField::FIELDS {
            let column = ModelODataMapper::map_field(*field).to_string();
            assert!(
                leads(&model_indexes, &column),
                "models field `{}` maps to unindexed column `{column}`",
                field.name()
            );
        }
        let provider_indexes = index_columns(&conn, "providers").await;
        for field in ProviderFilterField::FIELDS {
            let column = ProviderODataMapper::map_field(*field).to_string();
            assert!(
                leads(&provider_indexes, &column),
                "providers field `{}` maps to unindexed column `{column}`",
                field.name()
            );
        }
    }
}

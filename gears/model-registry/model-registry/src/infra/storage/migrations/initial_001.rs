use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let (uuid, bool_, tstz, jsonb_nullable, tstz_nullable) = match backend {
            sea_orm::DatabaseBackend::Postgres => {
                ("UUID", "BOOLEAN", "TIMESTAMPTZ", "JSONB", "TIMESTAMPTZ")
            }
            sea_orm::DatabaseBackend::MySql => (
                "VARCHAR(36)",
                "BOOLEAN",
                "DATETIME(6)",
                "JSON",
                "DATETIME(6)",
            ),
            sea_orm::DatabaseBackend::Sqlite => ("TEXT", "BOOLEAN", "TEXT", "TEXT", "TEXT"),
        };

        // VARCHAR(64) / TEXT — used for bounded short text fields (region, hosted_by,
        // reasoning_level, version, multiplier_display). On MySQL and Postgres we use
        // VARCHAR(64); on SQLite we use TEXT (SQLite has no VARCHAR length).
        let varch = match backend {
            sea_orm::DatabaseBackend::Sqlite => "TEXT",
            sea_orm::DatabaseBackend::MySql | sea_orm::DatabaseBackend::Postgres => "VARCHAR(64)",
        };

        // INT type — used for all integer columns. SQLite INTEGER is 8-byte and
        // accepts i64 cleanly. PostgreSQL `INTEGER` is a 32-bit signed type and
        // overflows for any `size_bytes` ≥ 2 GiB (which is the smallest modern LLM
        // weight file), so we use BIGINT there. MySQL `INTEGER` maps to INT (32-bit)
        // by default — we use BIGINT there too for the same reason.
        let int = match backend {
            sea_orm::DatabaseBackend::Sqlite => "INTEGER",
            sea_orm::DatabaseBackend::MySql | sea_orm::DatabaseBackend::Postgres => "BIGINT",
        };

        let sql = format!(
            r"
CREATE TABLE IF NOT EXISTS providers (
    id                          {uuid} NOT NULL,
    tenant_id                   {uuid} NOT NULL,
    slug                        VARCHAR(255) NOT NULL,
    name                        VARCHAR(255) NOT NULL,
    gts_type                    VARCHAR(255) NOT NULL,
    status                      VARCHAR(50) NOT NULL DEFAULT 'active',
    managed                     {bool_} NOT NULL DEFAULT 0,
    metadata                    {jsonb_nullable},
    discovery_enabled           {bool_} NOT NULL DEFAULT 0,
    discovery_interval_seconds  INTEGER,
    created_at                  {tstz} NOT NULL,
    updated_at                  {tstz} NOT NULL,
    PRIMARY KEY (id),
    UNIQUE (tenant_id, slug)
);

CREATE TABLE IF NOT EXISTS models (
    id                          {uuid} NOT NULL,
    provider_id                 {uuid} NOT NULL,
    tenant_id                   {uuid} NOT NULL,
    canonical_id                VARCHAR(255) NOT NULL,
    lifecycle_status            VARCHAR(50) NOT NULL,
    deprecated_at               {tstz_nullable},
    -- The `info` JSONB column has been dropped (2026-07-24). The 17 scalar columns
    -- below + 5 JSONB sub-object columns + `provider_settings` are the new source of truth.
    provider_settings           {jsonb_nullable},

    -- ════════════════════════════════════════════════════════════════════════
    -- 17 promoted scalar columns from `ModelInfoV1` (replacing `info`)
    -- ════════════════════════════════════════════════════════════════════════
    display_name                TEXT NOT NULL DEFAULT '',
    description                 TEXT,
    size_bytes                  {int},
    region                      {varch},
    hosted_by                   {varch},
    last_release_at             {tstz_nullable},
    reasoning_level             {varch},
    version                     {varch},
    sort_order                  {int},
    icon                        TEXT,
    multiplier_display          {varch},
    perf_response_latency_ms    {int},
    perf_tokens_per_second      {int},
    ctx_max_input_tokens        {int} NOT NULL DEFAULT 0,
    ctx_max_output_tokens       {int},
    ctx_output_vector_size      {int},
    allow_parameter_override    {bool_} NOT NULL DEFAULT 0,

    -- ════════════════════════════════════════════════════════════════════════
    -- 5 JSONB sub-object columns (replacing the rest of `info`)
    -- ════════════════════════════════════════════════════════════════════════
    -- Capability fields NOT promoted to scalar columns (everything in `ModelCapabilities`
    -- minus the 4 OData booleans stored as scalar columns above).
    capabilities_full           {jsonb_nullable},
    -- `DefaultInferenceParametersV1` sub-object.
    default_parameters          {jsonb_nullable},
    -- Forward-compat `additional_info` map.
    additional_info             {jsonb_nullable},
    -- Symmetric with `capabilities_full` for the `disabled_capabilities` map.
    disabled_capabilities_full  {jsonb_nullable},
    -- `allow_extra_params`: flat `Vec<String>` of caller-supplied parameter
    -- names permitted alongside the request (added 2026-07-24 to satisfy the
    -- `allow_extra_params` user decision in the plan).
    allow_extra_params          {jsonb_nullable},

    created_at                  {tstz} NOT NULL,
    updated_at                  {tstz} NOT NULL,
    gts_type                    VARCHAR(255),
    vendor                      VARCHAR(255),
    family                      VARCHAR(255),
    managed                     {bool_} NOT NULL DEFAULT 0,
    architecture                VARCHAR(255),
    format                      VARCHAR(255),
    provider_model_id           VARCHAR(255),
    supported_api               VARCHAR(50),
    approval_status             VARCHAR(50) NOT NULL DEFAULT 'pending',
    cap_vision                  {bool_} NOT NULL DEFAULT 0,
    cap_function_calling        {bool_} NOT NULL DEFAULT 0,
    cap_streaming               {bool_} NOT NULL DEFAULT 0,
    cap_reasoning_effort        {bool_} NOT NULL DEFAULT 0,
    PRIMARY KEY (id),
    UNIQUE (tenant_id, canonical_id),
    FOREIGN KEY (provider_id) REFERENCES providers(id) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS model_approvals (
    tenant_id                   {uuid} NOT NULL,
    model_id                    {uuid} NOT NULL,
    approval_status             VARCHAR(50) NOT NULL,
    created_at                  {tstz} NOT NULL,
    updated_at                  {tstz} NOT NULL,
    PRIMARY KEY (tenant_id, model_id),
    FOREIGN KEY (model_id) REFERENCES models(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_models_lifecycle_status ON models (lifecycle_status);
CREATE INDEX IF NOT EXISTS idx_models_approval_status  ON models (approval_status);
CREATE INDEX IF NOT EXISTS idx_models_gts_type         ON models (gts_type);
CREATE INDEX IF NOT EXISTS idx_models_vendor           ON models (vendor);
CREATE INDEX IF NOT EXISTS idx_models_family           ON models (family);
CREATE INDEX IF NOT EXISTS idx_models_architecture     ON models (architecture);
CREATE INDEX IF NOT EXISTS idx_models_format           ON models (format);
CREATE INDEX IF NOT EXISTS idx_models_provider_model_id ON models (provider_model_id);
CREATE INDEX IF NOT EXISTS idx_models_supported_api    ON models (supported_api);
CREATE INDEX IF NOT EXISTS idx_models_cap_vision       ON models (cap_vision);
CREATE INDEX IF NOT EXISTS idx_models_cap_fn_call      ON models (cap_function_calling);
CREATE INDEX IF NOT EXISTS idx_models_cap_streaming    ON models (cap_streaming);
CREATE INDEX IF NOT EXISTS idx_models_cap_reasoning    ON models (cap_reasoning_effort);
",
        );

        conn.execute_unprepared(&sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let sql = r"
DROP TABLE IF EXISTS model_approvals;
DROP TABLE IF EXISTS models;
DROP TABLE IF EXISTS providers;
";
        conn.execute_unprepared(sql).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::DbBackend;

    /// Apply the migration against an in-memory `SQLite` database and verify the
    /// tables are created then dropped.
    #[tokio::test]
    async fn initial_migration_up_down_roundtrip() {
        let conn = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite connection");
        let manager = SchemaManager::new(&conn);

        // Verify tables don't exist yet
        assert!(!manager.has_table("providers").await.unwrap());
        assert!(!manager.has_table("models").await.unwrap());
        assert!(!manager.has_table("model_approvals").await.unwrap());

        // Run migration up
        Migration::up(&Migration, &manager).await.unwrap();

        // Verify tables exist
        assert!(manager.has_table("providers").await.unwrap());
        assert!(manager.has_table("models").await.unwrap());
        assert!(manager.has_table("model_approvals").await.unwrap());

        // Validate schema by inserting a raw SQL row into providers
        conn.execute_unprepared(
            "INSERT INTO providers (id, tenant_id, slug, name, gts_type, status, managed, metadata, discovery_enabled, discovery_interval_seconds, created_at, updated_at) VALUES ('00000000-0000-0000-0000-000000000000', '00000000-0000-0000-0000-000000000000', 'test-provider', 'Test Provider', 'gts.cf.genai.models.provider.v1~', 'active', 0, NULL, 0, NULL, datetime('now'), datetime('now'))"
        ).await.expect("insert into providers should succeed");

        // Run migration down and verify tables are dropped
        Migration::down(&Migration, &manager).await.unwrap();
        assert!(!manager.has_table("model_approvals").await.unwrap());
        assert!(!manager.has_table("models").await.unwrap());
        assert!(!manager.has_table("providers").await.unwrap());
    }

    /// Verify the `info` column has been dropped from the `models` table.
    #[tokio::test]
    async fn models_info_column_absent() {
        let conn = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite connection");
        let manager = SchemaManager::new(&conn);
        Migration::up(&Migration, &manager).await.unwrap();

        // Query sqlite_master / pragma table_info for `models` columns.
        let rows = conn
            .query_all(sea_orm::Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA table_info(models);".to_owned(),
            ))
            .await
            .expect("pragma table_info");

        let column_names: Vec<String> = rows
            .iter()
            .filter_map(|row| row.try_get_by::<String, _>("name").ok())
            .collect();

        assert!(
            !column_names.iter().any(|n| n == "info"),
            "`info` column must be absent from `models`; found columns: {column_names:?}"
        );
    }

    /// Verify the 17 new scalar columns + 5 JSONB sub-object columns exist on `models`.
    #[tokio::test]
    async fn models_new_columns_present() {
        let conn = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite connection");
        let manager = SchemaManager::new(&conn);
        Migration::up(&Migration, &manager).await.unwrap();

        let rows = conn
            .query_all(sea_orm::Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA table_info(models);".to_owned(),
            ))
            .await
            .expect("pragma table_info");

        let columns: Vec<(String, String, i32, Option<String>)> = rows
            .iter()
            .map(|row| {
                (
                    row.try_get_by::<String, _>("name").unwrap_or_default(),
                    row.try_get_by::<String, _>("type").unwrap_or_default(),
                    row.try_get_by::<i32, _>("notnull").unwrap_or(0),
                    row.try_get_by::<String, _>("dflt_value").ok(),
                )
            })
            .collect();

        // (column_name, expected_type_substring, expected_notnull)
        let expected_scalars: &[(&str, &str, i32)] = &[
            ("display_name", "TEXT", 1),
            ("description", "TEXT", 0),
            ("size_bytes", "INTEGER", 0),
            ("region", "TEXT", 0),
            ("hosted_by", "TEXT", 0),
            ("last_release_at", "TEXT", 0),
            ("reasoning_level", "TEXT", 0),
            ("version", "TEXT", 0),
            ("sort_order", "INTEGER", 0),
            ("icon", "TEXT", 0),
            ("multiplier_display", "TEXT", 0),
            ("perf_response_latency_ms", "INTEGER", 0),
            ("perf_tokens_per_second", "INTEGER", 0),
            ("ctx_max_input_tokens", "INTEGER", 1),
            ("ctx_max_output_tokens", "INTEGER", 0),
            ("ctx_output_vector_size", "INTEGER", 0),
            ("allow_parameter_override", "BOOLEAN", 1),
        ];

        for (col_name, expected_type, expected_notnull) in expected_scalars {
            let actual = columns
                .iter()
                .find(|(name, _, _, _)| name == col_name)
                .unwrap_or_else(|| {
                    panic!(
                        "expected column `{col_name}` to exist; columns: {:?}",
                        columns.iter().map(|(n, _, _, _)| n).collect::<Vec<_>>()
                    )
                });
            assert!(
                actual.1.to_uppercase().contains(expected_type),
                "column `{col_name}` expected type containing `{expected_type}`, got `{}`",
                actual.1
            );
            assert_eq!(
                actual.2, *expected_notnull,
                "column `{col_name}` expected notnull={expected_notnull}, got {}",
                actual.2
            );
        }

        // 5 JSONB sub-object columns (TEXT on SQLite, nullable).
        for col_name in [
            "capabilities_full",
            "default_parameters",
            "additional_info",
            "disabled_capabilities_full",
            "allow_extra_params",
        ] {
            let actual = columns
                .iter()
                .find(|(name, _, _, _)| name == col_name)
                .unwrap_or_else(|| panic!("expected column `{col_name}` to exist"));
            assert_eq!(
                actual.1.to_uppercase(),
                "TEXT",
                "column `{col_name}` expected type TEXT, got `{}`",
                actual.1
            );
            assert_eq!(
                actual.2, 0,
                "column `{col_name}` expected nullable (notnull=0), got {}",
                actual.2
            );
        }
    }

    /// Verify NOT NULL DEFAULTs on the three required columns: `display_name`,
    /// `ctx_max_input_tokens`, `allow_parameter_override`. The defaults keep
    /// `SQLite` inserts cheap even before the application layer writes values.
    #[tokio::test]
    async fn models_required_defaults_present() {
        let conn = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite connection");
        let manager = SchemaManager::new(&conn);
        Migration::up(&Migration, &manager).await.unwrap();

        let rows = conn
            .query_all(sea_orm::Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA table_info(models);".to_owned(),
            ))
            .await
            .expect("pragma table_info");

        for row in rows {
            let name: String = row.try_get_by::<String, _>("name").unwrap_or_default();
            if !matches!(
                name.as_str(),
                "display_name" | "ctx_max_input_tokens" | "allow_parameter_override"
            ) {
                continue;
            }
            let dflt: Option<String> = row.try_get_by::<String, _>("dflt_value").ok();
            assert!(
                dflt.is_some(),
                "column `{name}` must have a DEFAULT; got none"
            );
        }
    }

    /// Verify the existing 11 indexes still exist after the migration rewrite.
    #[tokio::test]
    async fn models_indexes_preserved() {
        let conn = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite connection");
        let manager = SchemaManager::new(&conn);
        Migration::up(&Migration, &manager).await.unwrap();

        let rows = conn
            .query_all(sea_orm::Statement::from_string(
                DbBackend::Sqlite,
                "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='models' AND name LIKE 'idx_models_%';".to_owned(),
            ))
            .await
            .expect("query indexes");

        let names: Vec<String> = rows
            .iter()
            .filter_map(|row| row.try_get_by::<String, _>("name").ok())
            .collect();

        for expected in [
            "idx_models_lifecycle_status",
            "idx_models_approval_status",
            "idx_models_gts_type",
            "idx_models_vendor",
            "idx_models_family",
            "idx_models_architecture",
            "idx_models_format",
            "idx_models_provider_model_id",
            "idx_models_supported_api",
            "idx_models_cap_vision",
            "idx_models_cap_fn_call",
            "idx_models_cap_streaming",
            "idx_models_cap_reasoning",
        ] {
            assert!(
                names.iter().any(|n| n == expected),
                "expected index `{expected}`; got indexes: {names:?}"
            );
        }
    }
}

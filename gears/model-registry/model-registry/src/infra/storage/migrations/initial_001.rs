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
    info                        {jsonb_nullable},
    provider_settings           {jsonb_nullable},
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
    FOREIGN KEY (provider_id) REFERENCES providers(id)
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
}

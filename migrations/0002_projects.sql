-- W02 project setup fields and optimistic-concurrency revision.
-- Existing W01 tables remain compatible; all new fields have explicit
-- defaults so a rolling migration can preserve existing project rows.

ALTER TABLE projects
    ADD COLUMN IF NOT EXISTS brand_name TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS product_name TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS market TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS language TEXT NOT NULL DEFAULT 'en',
    ADD COLUMN IF NOT EXISTS target_audience TEXT,
    ADD COLUMN IF NOT EXISTS competitors JSONB NOT NULL DEFAULT '[]'::JSONB,
    ADD COLUMN IF NOT EXISTS initial_sources JSONB NOT NULL DEFAULT '[]'::JSONB,
    ADD COLUMN IF NOT EXISTS resource_mode TEXT NOT NULL DEFAULT 'own',
    ADD COLUMN IF NOT EXISTS budget_currency TEXT NOT NULL DEFAULT 'CNY',
    ADD COLUMN IF NOT EXISTS monthly_budget_minor BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS monitoring_reserve_percent SMALLINT NOT NULL DEFAULT 20,
    ADD COLUMN IF NOT EXISTS status TEXT NOT NULL DEFAULT 'draft',
    ADD COLUMN IF NOT EXISTS revision BIGINT NOT NULL DEFAULT 1;

ALTER TABLE projects
    DROP CONSTRAINT IF EXISTS projects_resource_mode_check,
    ADD CONSTRAINT projects_resource_mode_check
        CHECK (resource_mode IN ('own', 'platform', 'mixed')),
    DROP CONSTRAINT IF EXISTS projects_monthly_budget_minor_check,
    ADD CONSTRAINT projects_monthly_budget_minor_check
        CHECK (monthly_budget_minor >= 0),
    DROP CONSTRAINT IF EXISTS projects_monitoring_reserve_percent_check,
    ADD CONSTRAINT projects_monitoring_reserve_percent_check
        CHECK (monitoring_reserve_percent BETWEEN 0 AND 100),
    DROP CONSTRAINT IF EXISTS projects_status_check,
    ADD CONSTRAINT projects_status_check
        CHECK (status IN ('draft', 'active', 'paused', 'archived')),
    DROP CONSTRAINT IF EXISTS projects_revision_check,
    ADD CONSTRAINT projects_revision_check
        CHECK (revision > 0),
    DROP CONSTRAINT IF EXISTS projects_budget_currency_check,
    ADD CONSTRAINT projects_budget_currency_check
        CHECK (budget_currency ~ '^[A-Z]{3}$');

CREATE INDEX IF NOT EXISTS projects_scope_created_id_idx
    ON projects (operator_id, tenant_id, created_at, project_id);

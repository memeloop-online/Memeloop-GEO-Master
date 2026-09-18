-- W02 atomic project start.  This migration intentionally leaves 0001-0004
-- untouched: a start freezes inputs and creates only fan-out/reduce skeletons,
-- never knowledge items, document items, or distribution targets.

ALTER TABLE projects
    ADD COLUMN project_settings JSONB NOT NULL DEFAULT '{}'::JSONB,
    ADD COLUMN current_config_revision_id UUID,
    ADD COLUMN current_cycle_id UUID,
    ADD COLUMN start_operation_id UUID;

ALTER TABLE projects
    ALTER COLUMN product_name DROP NOT NULL;

ALTER TABLE operations
    ADD CONSTRAINT operations_scope_project_operation_unique
        UNIQUE (operator_id, tenant_id, project_id, operation_id);

CREATE TABLE project_config_revisions (
    config_revision_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    project_revision BIGINT NOT NULL CHECK (project_revision > 0),
    settings JSONB NOT NULL,
    source_refs JSONB NOT NULL,
    settings_hash TEXT NOT NULL,
    estimate_snapshot JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id, config_revision_id),
    UNIQUE (operator_id, tenant_id, project_id, project_revision)
);
CREATE INDEX project_config_revisions_scope_project_idx
    ON project_config_revisions (operator_id, tenant_id, project_id, created_at DESC);

CREATE TABLE optimization_cycles (
    cycle_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    config_revision_id UUID NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('not_started', 'awaiting_knowledge', 'running', 'completed', 'failed')),
    report_timezone TEXT NOT NULL,
    report_window_start_at TIMESTAMPTZ NOT NULL,
    report_window_end_at TIMESTAMPTZ NOT NULL,
    cutoff_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, config_revision_id)
        REFERENCES project_config_revisions (operator_id, tenant_id, project_id, config_revision_id)
        ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id, cycle_id)
);
CREATE INDEX optimization_cycles_scope_project_created_idx
    ON optimization_cycles (operator_id, tenant_id, project_id, created_at DESC);

CREATE TABLE document_manifests (
    manifest_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    cycle_id UUID NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    state TEXT NOT NULL CHECK (state IN ('awaiting_knowledge', 'planning', 'ready', 'closed')),
    sealed BOOLEAN NOT NULL DEFAULT false,
    expected_count BIGINT,
    scope_hash TEXT NOT NULL,
    input_refs JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, cycle_id)
        REFERENCES optimization_cycles (operator_id, tenant_id, project_id, cycle_id) ON DELETE CASCADE,
    CHECK ((NOT sealed AND expected_count IS NULL) OR (sealed AND expected_count IS NOT NULL AND expected_count >= 0)),
    UNIQUE (operator_id, tenant_id, project_id, cycle_id, revision),
    UNIQUE (operator_id, tenant_id, project_id, manifest_id)
);
CREATE INDEX document_manifests_scope_cycle_idx
    ON document_manifests (operator_id, tenant_id, project_id, cycle_id, revision DESC);

CREATE TABLE distribution_manifests (
    manifest_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    cycle_id UUID NOT NULL,
    document_manifest_id UUID NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    state TEXT NOT NULL CHECK (state IN ('awaiting_documents', 'planning', 'ready', 'closed')),
    sealed BOOLEAN NOT NULL DEFAULT false,
    expected_count BIGINT,
    scope_hash TEXT NOT NULL,
    input_refs JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, cycle_id)
        REFERENCES optimization_cycles (operator_id, tenant_id, project_id, cycle_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id, document_manifest_id)
        REFERENCES document_manifests (operator_id, tenant_id, project_id, manifest_id) ON DELETE RESTRICT,
    CHECK ((NOT sealed AND expected_count IS NULL) OR (sealed AND expected_count IS NOT NULL AND expected_count >= 0)),
    UNIQUE (operator_id, tenant_id, project_id, cycle_id, revision),
    UNIQUE (operator_id, tenant_id, project_id, manifest_id)
);
CREATE INDEX distribution_manifests_scope_cycle_idx
    ON distribution_manifests (operator_id, tenant_id, project_id, cycle_id, revision DESC);

CREATE TABLE workflow_runs (
    workflow_run_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    cycle_id UUID NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('project_start')),
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'succeeded', 'failed')),
    input_refs JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, cycle_id)
        REFERENCES optimization_cycles (operator_id, tenant_id, project_id, cycle_id) ON DELETE CASCADE,
    UNIQUE (operator_id, tenant_id, project_id, cycle_id, kind)
);
CREATE INDEX workflow_runs_scope_cycle_idx
    ON workflow_runs (operator_id, tenant_id, project_id, cycle_id, created_at DESC);

CREATE TABLE project_start_records (
    project_start_record_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    operation_id UUID NOT NULL,
    cycle_id UUID NOT NULL,
    config_revision_id UUID NOT NULL,
    document_manifest_id UUID NOT NULL,
    distribution_manifest_id UUID NOT NULL,
    idempotency_key_hash TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    FOREIGN KEY (operation_id) REFERENCES operations (operation_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, cycle_id)
        REFERENCES optimization_cycles (operator_id, tenant_id, project_id, cycle_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, config_revision_id)
        REFERENCES project_config_revisions (operator_id, tenant_id, project_id, config_revision_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, document_manifest_id)
        REFERENCES document_manifests (operator_id, tenant_id, project_id, manifest_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, distribution_manifest_id)
        REFERENCES distribution_manifests (operator_id, tenant_id, project_id, manifest_id) ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id),
    UNIQUE (operator_id, tenant_id, project_id, idempotency_key_hash)
);
CREATE INDEX project_start_records_scope_project_idx
    ON project_start_records (operator_id, tenant_id, project_id);

ALTER TABLE projects
    ADD CONSTRAINT projects_current_config_revision_scope_fk
        FOREIGN KEY (operator_id, tenant_id, project_id, current_config_revision_id)
        REFERENCES project_config_revisions (operator_id, tenant_id, project_id, config_revision_id)
        DEFERRABLE INITIALLY DEFERRED,
    ADD CONSTRAINT projects_current_cycle_scope_fk
        FOREIGN KEY (operator_id, tenant_id, project_id, current_cycle_id)
        REFERENCES optimization_cycles (operator_id, tenant_id, project_id, cycle_id)
        DEFERRABLE INITIALLY DEFERRED,
    ADD CONSTRAINT projects_start_operation_scope_fk
        FOREIGN KEY (operator_id, tenant_id, project_id, start_operation_id)
        REFERENCES operations (operator_id, tenant_id, project_id, operation_id)
        DEFERRABLE INITIALLY DEFERRED;

CREATE INDEX projects_start_handles_idx
    ON projects (operator_id, tenant_id, current_cycle_id)
    WHERE current_cycle_id IS NOT NULL;

-- First fan-out execution is separate from the frozen planning manifest.
-- One scoped aggregate row is locked for every transition; immutable outputs
-- are also recorded in append-only relational tables.
CREATE TABLE content_executions (
    execution_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    cycle_id UUID NOT NULL,
    manifest_id UUID NOT NULL,
    manifest_revision INTEGER NOT NULL,
    policy_version TEXT NOT NULL,
    input_hash TEXT NOT NULL,
    state JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id, project_id, cycle_id)
        REFERENCES optimization_cycles (operator_id, tenant_id, project_id, cycle_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, manifest_id)
        REFERENCES document_manifests (operator_id, tenant_id, project_id, manifest_id) ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id, execution_id),
    UNIQUE (operator_id, tenant_id, project_id, cycle_id, manifest_id, manifest_revision, policy_version)
);
CREATE INDEX content_executions_cycle_idx ON content_executions
    (operator_id, tenant_id, project_id, cycle_id);

CREATE TABLE content_briefs (
    brief_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    execution_id UUID NOT NULL,
    item_id UUID NOT NULL,
    body JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (operator_id, tenant_id, project_id, execution_id)
        REFERENCES content_executions (operator_id, tenant_id, project_id, execution_id) ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id, execution_id, item_id)
);

CREATE TABLE content_revisions (
    revision_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    execution_id UUID NOT NULL,
    asset_id UUID NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    body JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (operator_id, tenant_id, project_id, execution_id)
        REFERENCES content_executions (operator_id, tenant_id, project_id, execution_id) ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id, asset_id, revision)
);
CREATE INDEX content_revisions_asset_idx ON content_revisions
    (operator_id, tenant_id, project_id, asset_id, revision);

CREATE TABLE content_checks (
    check_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    execution_id UUID NOT NULL,
    revision_id UUID NOT NULL,
    body JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (operator_id, tenant_id, project_id, execution_id)
        REFERENCES content_executions (operator_id, tenant_id, project_id, execution_id) ON DELETE RESTRICT,
    FOREIGN KEY (revision_id) REFERENCES content_revisions (revision_id) ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id, revision_id)
);

CREATE TABLE content_handoffs (
    handoff_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    execution_id UUID NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    supersedes_handoff_id UUID,
    body JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (operator_id, tenant_id, project_id, execution_id)
        REFERENCES content_executions (operator_id, tenant_id, project_id, execution_id) ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id, execution_id, revision),
    FOREIGN KEY (supersedes_handoff_id) REFERENCES content_handoffs (handoff_id) ON DELETE RESTRICT
);

CREATE FUNCTION reject_content_immutable_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'content revisions and handoffs are immutable';
END $$;
CREATE TRIGGER content_revisions_immutable BEFORE UPDATE OR DELETE ON content_revisions
    FOR EACH ROW EXECUTE FUNCTION reject_content_immutable_change();
CREATE TRIGGER content_briefs_immutable BEFORE UPDATE OR DELETE ON content_briefs
    FOR EACH ROW EXECUTE FUNCTION reject_content_immutable_change();
CREATE TRIGGER content_checks_immutable BEFORE UPDATE OR DELETE ON content_checks
    FOR EACH ROW EXECUTE FUNCTION reject_content_immutable_change();
CREATE TRIGGER content_handoffs_immutable BEFORE UPDATE OR DELETE ON content_handoffs
    FOR EACH ROW EXECUTE FUNCTION reject_content_immutable_change();

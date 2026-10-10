-- Semantic reuse is scoped to one project. The descriptor is retained with
-- its digest so a collision or a changed canonicalization cannot alias input.
-- Reservation is fenced by both a token and an expiry; only checked, current
-- revisions can become candidates. Old executions are never rewritten.
ALTER TABLE content_revisions ADD COLUMN derived_from_revision_id UUID;
ALTER TABLE content_revisions ADD CONSTRAINT content_revisions_derived_scope_fk
    FOREIGN KEY (operator_id, tenant_id, project_id, derived_from_revision_id)
    REFERENCES content_revisions (operator_id, tenant_id, project_id, revision_id) ON DELETE RESTRICT;
CREATE UNIQUE INDEX content_checks_reuse_scope_idx ON content_checks
    (operator_id, tenant_id, project_id, check_id);
CREATE TABLE content_reuse_registry (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    fingerprint TEXT NOT NULL CHECK (fingerprint ~ '^[0-9a-f]{64}$'),
    descriptor JSONB NOT NULL,
    origin_execution_id UUID,
    origin_item_id UUID,
    asset_id UUID,
    revision_id UUID,
    check_id UUID,
    reservation_execution_id UUID,
    reservation_item_id UUID,
    reservation_token UUID,
    reservation_expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (operator_id, tenant_id, project_id, fingerprint),
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, origin_execution_id)
        REFERENCES content_executions (operator_id, tenant_id, project_id, execution_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, reservation_execution_id)
        REFERENCES content_executions (operator_id, tenant_id, project_id, execution_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, revision_id)
        REFERENCES content_revisions (operator_id, tenant_id, project_id, revision_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, check_id)
        REFERENCES content_checks (operator_id, tenant_id, project_id, check_id) ON DELETE RESTRICT,
    CHECK ((origin_execution_id IS NULL AND origin_item_id IS NULL AND asset_id IS NULL AND revision_id IS NULL AND check_id IS NULL)
        OR (origin_execution_id IS NOT NULL AND origin_item_id IS NOT NULL AND asset_id IS NOT NULL AND revision_id IS NOT NULL AND check_id IS NOT NULL)),
    CHECK ((reservation_execution_id IS NULL AND reservation_item_id IS NULL AND reservation_token IS NULL AND reservation_expires_at IS NULL)
        OR (reservation_execution_id IS NOT NULL AND reservation_item_id IS NOT NULL AND reservation_token IS NOT NULL AND reservation_expires_at IS NOT NULL))
);
-- Reuse bindings are immutable provenance; the destination aggregate's item
-- also includes the binding for existing read APIs and handoff closure.
CREATE TABLE content_reuse_bindings (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    execution_id UUID NOT NULL,
    item_id UUID NOT NULL,
    fingerprint TEXT NOT NULL,
    origin_execution_id UUID NOT NULL,
    origin_item_id UUID NOT NULL,
    asset_id UUID NOT NULL,
    revision_id UUID NOT NULL,
    check_id UUID NOT NULL,
    reused_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (operator_id, tenant_id, project_id, execution_id, item_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, fingerprint)
        REFERENCES content_reuse_registry (operator_id, tenant_id, project_id, fingerprint) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, execution_id)
        REFERENCES content_executions (operator_id, tenant_id, project_id, execution_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, origin_execution_id)
        REFERENCES content_executions (operator_id, tenant_id, project_id, execution_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, revision_id)
        REFERENCES content_revisions (operator_id, tenant_id, project_id, revision_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, check_id)
        REFERENCES content_checks (operator_id, tenant_id, project_id, check_id) ON DELETE RESTRICT
);
CREATE INDEX content_reuse_registry_origin_idx ON content_reuse_registry
    (operator_id, tenant_id, project_id, origin_execution_id)
    WHERE origin_execution_id IS NOT NULL;
-- One-time index of pre-descriptor branches. Presence without a proven
-- descriptor/check must block automatic re-publication; lookup never scans
-- historical execution JSON for each incoming branch.
CREATE TABLE content_reuse_legacy_branches (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    execution_id UUID NOT NULL,
    item_id UUID NOT NULL,
    document_key TEXT NOT NULL,
    source_version_refs JSONB NOT NULL,
    PRIMARY KEY (operator_id, tenant_id, project_id, execution_id, item_id)
);
INSERT INTO content_reuse_legacy_branches
    (operator_id,tenant_id,project_id,execution_id,item_id,document_key,source_version_refs)
SELECT c.operator_id,c.tenant_id,c.project_id,c.execution_id,
       (i.item->>'item_id')::uuid,i.item->>'document_key',i.item->'source_version_refs'
FROM content_executions c CROSS JOIN LATERAL jsonb_array_elements(c.state->'items') AS i(item)
WHERE i.item->>'status'='ready'
    AND jsonb_typeof(i.item->'source_version_refs')='array'
    AND i.item->>'document_key' IS NOT NULL;
CREATE INDEX content_reuse_legacy_lookup_idx ON content_reuse_legacy_branches
    (operator_id,tenant_id,project_id,document_key);
CREATE TRIGGER content_reuse_bindings_immutable BEFORE UPDATE OR DELETE ON content_reuse_bindings
    FOR EACH ROW EXECUTE FUNCTION reject_content_immutable_change();

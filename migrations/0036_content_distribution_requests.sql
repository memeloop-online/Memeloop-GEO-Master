-- Single-article acceptance is separate from frozen cycle coverage and from
-- the existing publication attempt/outbox state machine.
CREATE TABLE content_distribution_requests (
    request_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    schema_version INTEGER NOT NULL CHECK (schema_version = 1),
    content_revision_id UUID NOT NULL,
    content_asset_id UUID NOT NULL,
    platform_id TEXT NOT NULL,
    placement_slot TEXT NOT NULL,
    account_id UUID NOT NULL,
    account_owner_kind TEXT NOT NULL CHECK (account_owner_kind IN ('customer', 'operator_pool')),
    format TEXT NOT NULL CHECK (format IN ('markdown.v1', 'rich_markdown.v2')),
    idempotency_key_hash TEXT NOT NULL CHECK (length(idempotency_key_hash) = 64),
    request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
    publication_intent_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (operator_id, tenant_id, project_id, request_id),
    UNIQUE (operator_id, tenant_id, project_id, idempotency_key_hash),
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, content_revision_id)
        REFERENCES content_revisions (operator_id, tenant_id, project_id, revision_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, publication_intent_id)
        REFERENCES distribution_publication_intents (operator_id, tenant_id, project_id, intent_id) ON DELETE RESTRICT
);
CREATE INDEX content_distribution_requests_intent_idx
    ON content_distribution_requests (operator_id, tenant_id, project_id, publication_intent_id)
    WHERE publication_intent_id IS NOT NULL;

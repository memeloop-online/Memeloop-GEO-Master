-- Interpretation revisions are separate from original immutable measurements.
-- The queued intent commits before any external inference is attempted.
CREATE TABLE observation_analyses (
    revision_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    target_id UUID NOT NULL,
    attempt_id UUID NOT NULL,
    idempotency_key_hash CHAR(64) NOT NULL,
    request_digest CHAR(64) NOT NULL,
    request JSONB NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'completed')),
    claim_token UUID,
    created_at TIMESTAMPTZ NOT NULL,
    started_at TIMESTAMPTZ,
    analyzed_at TIMESTAMPTZ,
    result JSONB,
    UNIQUE (operator_id, tenant_id, project_id, idempotency_key_hash),
    FOREIGN KEY (operator_id, tenant_id, project_id, target_id)
        REFERENCES channel_execution_targets (operator_id, tenant_id, project_id, target_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, attempt_id)
        REFERENCES channel_execution_attempts (operator_id, tenant_id, project_id, attempt_id),
    CHECK (octet_length(request::text) <= 8192),
    CHECK (result IS NULL OR octet_length(result::text) <= 1000000),
    CHECK (started_at IS NULL OR started_at >= created_at),
    CHECK (analyzed_at IS NULL OR analyzed_at >= started_at),
    CHECK (
        (state='queued' AND claim_token IS NULL AND started_at IS NULL AND analyzed_at IS NULL AND result IS NULL)
        OR (state='running' AND claim_token IS NOT NULL AND started_at IS NOT NULL AND analyzed_at IS NULL AND result IS NULL)
        OR (state='completed' AND claim_token IS NOT NULL AND started_at IS NOT NULL AND analyzed_at IS NOT NULL AND result IS NOT NULL)
    )
);
CREATE INDEX observation_analyses_attempt_page
    ON observation_analyses (operator_id, tenant_id, project_id, target_id, attempt_id, revision_id);

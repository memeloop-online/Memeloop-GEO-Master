-- Scheduling ledger only; immutable captures remain the sole ownership proof.
CREATE TABLE provider_conversation_cleanup (
    cleanup_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    capture_id UUID NOT NULL,
    account_id UUID NOT NULL,
    provider TEXT NOT NULL CHECK (length(provider) BETWEEN 1 AND 64),
    external_conversation_id TEXT NOT NULL CHECK (length(external_conversation_id) BETWEEN 1 AND 128),
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending','running','deleted','archived','unknown','failed','needs_login')),
    action TEXT CHECK (action IN ('delete','reconcile')),
    lease_id UUID,
    lease_until TIMESTAMPTZ,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count BETWEEN 0 AND 1000),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (operator_id, account_id, provider, external_conversation_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, capture_id)
        REFERENCES observation_captures(operator_id, tenant_id, project_id, capture_id)
        ON DELETE RESTRICT,
    CHECK ((state = 'running') = (lease_id IS NOT NULL AND lease_until IS NOT NULL)),
    CHECK (state <> 'running' OR action IS NOT NULL)
);
CREATE INDEX provider_cleanup_due_idx
    ON provider_conversation_cleanup(operator_id, tenant_id, project_id, next_attempt_at, cleanup_id)
    WHERE state NOT IN ('deleted','archived');
CREATE INDEX observation_capture_owned_conversation_idx
    ON observation_captures(operator_id, account_id,
        (input #>> '{owned_conversation,provider}'),
        (input #>> '{owned_conversation,external_conversation_id}'));

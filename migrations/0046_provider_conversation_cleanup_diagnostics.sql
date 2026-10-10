-- No provider text, account identity or remote response belongs in diagnostics.
CREATE FUNCTION provider_cleanup_diagnostic_valid(value JSONB) RETURNS BOOLEAN
LANGUAGE SQL IMMUTABLE AS $$
    SELECT value IS NULL OR (
        jsonb_typeof(value) = 'object'
        AND value ? 'stage' AND value ? 'code'
        AND value - 'stage' - 'code' = '{}'::jsonb
        AND value->>'stage' IN ('scope','identity','inspection','authorization','messages',
            'inventory','delete','deadline','runner','preflight')
        AND value->>'code' IN ('invalid_scope','unsupported_platform','reauth_required',
            'account_mismatch','wrong_origin','invalid_response','http_error','too_large',
            'transport_unknown','chat_mismatch','generating','authorization_required',
            'authorization_expired','unverified_messages','pagination_incomplete',
            'message_inventory_mismatch','unverified_delete_response','deadline_exceeded',
            'dependency_unavailable','account_busy','retained_evidence_required')
    ) IS TRUE
$$;

ALTER TABLE provider_conversation_cleanup ADD COLUMN last_diagnostic JSONB
    CHECK (provider_cleanup_diagnostic_valid(last_diagnostic));

CREATE TABLE provider_conversation_cleanup_attempts (
    cleanup_id UUID NOT NULL REFERENCES provider_conversation_cleanup(cleanup_id) ON DELETE RESTRICT,
    lease_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    action TEXT NOT NULL CHECK (action IN ('delete','reconcile')),
    state TEXT NOT NULL CHECK (state IN ('deleted','archived','unknown','failed','needs_login','pending')),
    diagnostic JSONB CHECK (provider_cleanup_diagnostic_valid(diagnostic)),
    finished_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX provider_cleanup_attempts_scope_idx ON provider_conversation_cleanup_attempts
    (operator_id,tenant_id,project_id,cleanup_id,finished_at);

CREATE FUNCTION reject_provider_cleanup_attempt_mutation() RETURNS TRIGGER
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'cleanup attempt summaries are immutable';
END;
$$;
CREATE TRIGGER provider_cleanup_attempts_immutable
    BEFORE UPDATE OR DELETE ON provider_conversation_cleanup_attempts
    FOR EACH ROW EXECUTE FUNCTION reject_provider_cleanup_attempt_mutation();

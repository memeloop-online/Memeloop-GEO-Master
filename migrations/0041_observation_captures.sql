-- Immutable, bounded raw observation checkpoints precede AI interpretation.
-- A capture is not a verified outcome or a remote cleanup authorization.
ALTER TABLE channel_execution_attempts
    ADD CONSTRAINT channel_attempt_capture_account_key
    UNIQUE (operator_id, tenant_id, project_id, target_id, attempt_id, account_id);

CREATE TABLE observation_captures (
    capture_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    target_id UUID NOT NULL,
    attempt_id UUID NOT NULL,
    account_id UUID NOT NULL,
    runner_session_id UUID NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    phase TEXT NOT NULL CHECK (phase IN ('source', 'extraction', 'candidate')),
    source_capture_id UUID,
    input_hash CHAR(64) NOT NULL,
    input JSONB NOT NULL,
    stored_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (operator_id, tenant_id, project_id, attempt_id, runner_session_id, ordinal),
    UNIQUE (operator_id, tenant_id, project_id, capture_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, target_id, attempt_id, account_id)
        REFERENCES channel_execution_attempts
            (operator_id, tenant_id, project_id, target_id, attempt_id, account_id)
        ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, source_capture_id)
        REFERENCES observation_captures (operator_id, tenant_id, project_id, capture_id)
        ON DELETE RESTRICT,
    CHECK ((phase = 'source' AND ordinal = 0 AND source_capture_id IS NULL)
        OR (phase IN ('extraction', 'candidate') AND ordinal > 0 AND source_capture_id IS NOT NULL)),
    -- source_json is itself a JSON string: nesting can double its 750 kB
    -- validated byte length through escaping, plus the capture envelope.
    CHECK (octet_length(input::text) <= 2000000)
);
CREATE INDEX observation_captures_attempt_idx
    ON observation_captures (operator_id, tenant_id, project_id, attempt_id, ordinal);

-- Database-enforced write-once even if a future caller bypasses this repository.
CREATE FUNCTION reject_observation_capture_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'observation captures are immutable';
END;
$$;
CREATE TRIGGER observation_captures_immutable
    BEFORE UPDATE OR DELETE ON observation_captures
    FOR EACH ROW EXECUTE FUNCTION reject_observation_capture_change();

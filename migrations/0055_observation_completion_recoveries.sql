-- History recovery remains separate from the immutable original capture.
-- Neither this record nor its receipt grants remote deletion authority.
CREATE TABLE observation_completion_recoveries (
    recovery_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    source_capture_id UUID NOT NULL,
    recovery_execution_id UUID NOT NULL,
    input_hash TEXT NOT NULL CHECK (input_hash ~ '^[0-9a-f]{64}$'),
    input JSONB NOT NULL CHECK (jsonb_typeof(input) = 'object'),
    stored_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (operator_id, tenant_id, project_id, source_capture_id, recovery_execution_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, source_capture_id)
        REFERENCES observation_captures (operator_id, tenant_id, project_id, capture_id)
        ON DELETE RESTRICT,
    CHECK (octet_length(input::text) <= 2000000),
    CHECK (input->>'recovery_id' IS NOT NULL AND input->>'recovery_id' = recovery_id::text),
    CHECK (input->>'source_capture_id' IS NOT NULL AND input->>'source_capture_id' = source_capture_id::text),
    CHECK (input->>'recovery_execution_id' IS NOT NULL AND input->>'recovery_execution_id' = recovery_execution_id::text),
    CHECK (input->>'history_sha256' IS NOT NULL AND input->>'history_sha256' ~ '^[0-9a-f]{64}$'),
    CHECK (input->>'source_sha256' IS NOT NULL AND input->>'source_sha256' ~ '^[0-9a-f]{64}$'),
    CHECK (input->>'source_receipt_sha256' IS NOT NULL AND input->>'source_receipt_sha256' ~ '^[0-9a-f]{64}$'),
    CHECK (input->>'history_json' IS NOT NULL AND octet_length(input->>'history_json') BETWEEN 1 AND 750000)
);

CREATE FUNCTION reject_observation_completion_recovery_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'observation completion recoveries are immutable';
END;
$$;
CREATE TRIGGER observation_completion_recoveries_immutable
    BEFORE UPDATE OR DELETE ON observation_completion_recoveries
    FOR EACH ROW EXECUTE FUNCTION reject_observation_completion_recovery_change();

-- Read-only lookup ledger. These tables never update the original send attempt.
CREATE TABLE publication_lookup_jobs (
    attempt_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    target_id UUID NOT NULL,
    account_id UUID NOT NULL,
    frozen_input JSONB NOT NULL,
    connector_version TEXT,
    candidate_public_url TEXT,
    next_due_at TIMESTAMPTZ,
    lease_execution_id UUID,
    lease_expires_at TIMESTAMPTZ,
    query_count INTEGER NOT NULL DEFAULT 0 CHECK (query_count >= 0),
    last_error_code TEXT,
    created_at TIMESTAMPTZ NOT NULL,
    CHECK ((lease_execution_id IS NULL) = (lease_expires_at IS NULL)),
    CHECK (candidate_public_url IS NULL OR connector_version IS NOT NULL),
    UNIQUE (operator_id, tenant_id, project_id, attempt_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, attempt_id)
        REFERENCES channel_execution_attempts (operator_id, tenant_id, project_id, attempt_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, target_id)
        REFERENCES channel_execution_targets (operator_id, tenant_id, project_id, target_id) ON DELETE RESTRICT
);

CREATE INDEX publication_lookup_due_idx
    ON publication_lookup_jobs (attempt_id)
    WHERE next_due_at IS NOT NULL;

-- Execution IDs are single-use even after an expired lease was replaced.
CREATE TABLE publication_lookup_executions (
    execution_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    attempt_id UUID NOT NULL,
    claimed_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    CHECK (expires_at > claimed_at),
    UNIQUE (operator_id, tenant_id, project_id, execution_id),
    UNIQUE (operator_id, tenant_id, project_id, attempt_id, execution_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, attempt_id)
        REFERENCES publication_lookup_jobs (operator_id, tenant_id, project_id, attempt_id) ON DELETE RESTRICT
);

CREATE TABLE publication_lookup_observations (
    execution_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    attempt_id UUID NOT NULL,
    finding TEXT NOT NULL CHECK (finding IN ('unknown', 'asset_observed')),
    evidence JSONB NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL,
    received_at TIMESTAMPTZ NOT NULL,
    error_code TEXT,
    next_due_at TIMESTAMPTZ,
    CHECK (observed_at <= received_at),
    CHECK (next_due_at IS NULL OR next_due_at > received_at),
    UNIQUE (operator_id, tenant_id, project_id, execution_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, attempt_id, execution_id)
        REFERENCES publication_lookup_executions (operator_id, tenant_id, project_id, attempt_id, execution_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, attempt_id)
        REFERENCES publication_lookup_jobs (operator_id, tenant_id, project_id, attempt_id) ON DELETE RESTRICT
);

CREATE INDEX publication_lookup_observations_attempt_idx
    ON publication_lookup_observations (operator_id, tenant_id, project_id, attempt_id, received_at, execution_id);

CREATE FUNCTION publication_lookup_prevent_history_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'publication lookup history is append only';
END
$$;
CREATE TRIGGER publication_lookup_executions_immutable
    BEFORE UPDATE OR DELETE ON publication_lookup_executions
    FOR EACH ROW EXECUTE FUNCTION publication_lookup_prevent_history_mutation();
CREATE TRIGGER publication_lookup_observations_immutable
    BEFORE UPDATE OR DELETE ON publication_lookup_observations
    FOR EACH ROW EXECUTE FUNCTION publication_lookup_prevent_history_mutation();

-- Independent scheduling for read-only recovery. No capture, cleanup, account
-- reservation or browser-disposal state is rewritten by this queue.
CREATE TABLE observation_completion_recovery_jobs (
    job_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    source_capture_id UUID NOT NULL,
    source_receipt_sha256 TEXT NOT NULL CHECK (source_receipt_sha256 ~ '^[0-9a-f]{64}$'),
    state TEXT NOT NULL CHECK (state IN ('pending','running','retry','needs_login','recovered','retained')),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count BETWEEN 0 AND 1000000),
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_id UUID,
    lease_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (operator_id,tenant_id,project_id,source_capture_id),
    UNIQUE (operator_id,tenant_id,project_id,job_id),
    FOREIGN KEY (operator_id,tenant_id,project_id,source_capture_id)
        REFERENCES observation_captures (operator_id,tenant_id,project_id,capture_id) ON DELETE RESTRICT,
    CHECK ((state='running') = (lease_id IS NOT NULL AND lease_until IS NOT NULL)),
    CHECK ((lease_id IS NULL) = (lease_until IS NULL))
);
CREATE INDEX observation_completion_recovery_jobs_due
    ON observation_completion_recovery_jobs (job_id,next_attempt_at)
    WHERE state NOT IN ('recovered','retained');

-- Append-only claim/outcome events survive process death and expired lease
-- reclamation. "expired" never certifies that a browser was disposed.
CREATE TABLE observation_completion_recovery_job_attempts (
    event_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    job_id UUID NOT NULL,
    lease_id UUID NOT NULL,
    attempt_number INTEGER NOT NULL CHECK (attempt_number BETWEEN 1 AND 1000000),
    event TEXT NOT NULL CHECK (event IN ('claimed','expired','recovered','retry','needs_login','retained')),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (job_id,lease_id,event),
    FOREIGN KEY (operator_id,tenant_id,project_id,job_id)
        REFERENCES observation_completion_recovery_jobs (operator_id,tenant_id,project_id,job_id) ON DELETE RESTRICT
);
CREATE FUNCTION reject_observation_completion_recovery_attempt_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'observation completion recovery attempts are immutable';
END;
$$;
CREATE TRIGGER observation_completion_recovery_job_attempts_immutable
    BEFORE UPDATE OR DELETE ON observation_completion_recovery_job_attempts
    FOR EACH ROW EXECUTE FUNCTION reject_observation_completion_recovery_attempt_change();

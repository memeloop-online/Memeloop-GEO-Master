-- Independent SERP tasks do not reuse AI observations or account captures.
CREATE TABLE serp_measurements (
    measurement_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    idempotency_key_hash CHAR(64) NOT NULL,
    measurement JSONB NOT NULL,
    state TEXT NOT NULL CHECK (state IN (
        'queued','claimed','sending','awaiting_result','completed','unknown','failed','cancelled'
    )),
    created_at TIMESTAMPTZ NOT NULL,
    scheduled_at TIMESTAMPTZ NOT NULL,
    stored_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    claim JSONB,
    lease_expires_at TIMESTAMPTZ,
    next_poll_at TIMESTAMPTZ,
    sending_intent JSONB,
    provider_task JSONB,
    UNIQUE (operator_id, tenant_id, project_id, measurement_id),
    UNIQUE (operator_id, tenant_id, project_id, idempotency_key_hash),
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id),
    CHECK (provider_task IS NULL OR sending_intent IS NOT NULL)
);
CREATE INDEX serp_measurements_page
    ON serp_measurements (operator_id, tenant_id, project_id, created_at DESC, measurement_id DESC);
CREATE INDEX serp_measurements_expired
    ON serp_measurements (operator_id, tenant_id, project_id, lease_expires_at, measurement_id)
    WHERE state IN ('claimed','sending','awaiting_result') AND lease_expires_at IS NOT NULL;
CREATE INDEX serp_measurements_due
    ON serp_measurements (operator_id, tenant_id, project_id, measurement_id)
    INCLUDE (state, scheduled_at, next_poll_at, lease_expires_at)
    WHERE state IN ('queued','awaiting_result','unknown');

-- Initial ownership receipts survive expiry/reclaim. Current lease authority
-- remains exclusively on the locked measurement row.
CREATE TABLE serp_claims (
    claim_token UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    measurement_id UUID NOT NULL,
    attempt_id UUID NOT NULL,
    claim JSONB NOT NULL,
    stored_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    FOREIGN KEY (operator_id, tenant_id, project_id, measurement_id)
        REFERENCES serp_measurements (operator_id, tenant_id, project_id, measurement_id)
);
CREATE INDEX serp_claims_attempt
    ON serp_claims (operator_id, tenant_id, project_id, measurement_id, attempt_id);

CREATE TABLE serp_raw_evidence (
    evidence_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    measurement_id UUID NOT NULL,
    attempt_id UUID NOT NULL,
    metadata JSONB NOT NULL,
    body BYTEA NOT NULL,
    response_sha256 CHAR(64) NOT NULL,
    stored_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (operator_id, tenant_id, project_id, evidence_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, measurement_id)
        REFERENCES serp_measurements (operator_id, tenant_id, project_id, measurement_id),
    CHECK (octet_length(body) <= 4194304)
);
CREATE INDEX serp_raw_evidence_attempt
    ON serp_raw_evidence (operator_id, tenant_id, project_id, measurement_id, attempt_id);
CREATE INDEX serp_raw_evidence_page
    ON serp_raw_evidence (operator_id, tenant_id, project_id, measurement_id, stored_at DESC, evidence_id DESC);

CREATE TABLE serp_observations (
    observation_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    measurement_id UUID NOT NULL,
    attempt_id UUID NOT NULL,
    raw_evidence_id UUID NOT NULL,
    observation JSONB NOT NULL,
    analyzed_at TIMESTAMPTZ NOT NULL,
    stored_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    FOREIGN KEY (operator_id, tenant_id, project_id, measurement_id)
        REFERENCES serp_measurements (operator_id, tenant_id, project_id, measurement_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, raw_evidence_id)
        REFERENCES serp_raw_evidence (operator_id, tenant_id, project_id, evidence_id)
);
CREATE INDEX serp_observations_page
    ON serp_observations (
        operator_id, tenant_id, project_id, measurement_id, analyzed_at DESC, observation_id DESC
    );

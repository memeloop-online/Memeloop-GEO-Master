-- Frozen external-channel denominator and append-only attempt identity.
-- No credential/session material is placed in these tables.
CREATE TABLE channel_execution_plans (
    plan_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    cycle_id UUID NOT NULL,
    input_hash TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    plan JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    UNIQUE (operator_id, tenant_id, project_id, cycle_id),
    UNIQUE (operator_id, tenant_id, project_id, plan_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, cycle_id)
        REFERENCES optimization_cycles (operator_id, tenant_id, project_id, cycle_id) ON DELETE RESTRICT
);

CREATE TABLE channel_execution_targets (
    target_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    cycle_id UUID NOT NULL,
    plan_id UUID NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('publish', 'measure')),
    frozen_input JSONB NOT NULL,
    ordinal INTEGER NOT NULL,
    UNIQUE (operator_id, tenant_id, project_id, target_id),
    UNIQUE (operator_id, tenant_id, project_id, plan_id, ordinal),
    FOREIGN KEY (operator_id, tenant_id, project_id, plan_id)
        REFERENCES channel_execution_plans (operator_id, tenant_id, project_id, plan_id) ON DELETE RESTRICT
);

CREATE TABLE channel_execution_attempts (
    attempt_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    target_id UUID NOT NULL,
    account_id UUID NOT NULL,
    target_kind TEXT NOT NULL CHECK (target_kind IN ('publish','measure')),
    claimed_at TIMESTAMPTZ NOT NULL,
    outcome JSONB,
    received_at TIMESTAMPTZ,
    CHECK ((outcome IS NULL) = (received_at IS NULL)),
    UNIQUE (operator_id, tenant_id, project_id, attempt_id),
    -- This initial connector slice cannot prove non-publication after an
    -- ambiguous send; no retry is permitted until a real lookup protocol exists.
    UNIQUE (operator_id, tenant_id, project_id, target_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, target_id)
        REFERENCES channel_execution_targets (operator_id, tenant_id, project_id, target_id) ON DELETE RESTRICT
);
-- Account writing is single-flight even when different publication targets
-- are claimed concurrently. Measurements may be parallel across samples.
CREATE UNIQUE INDEX channel_publish_account_active_idx
    ON channel_execution_attempts (operator_id, account_id)
    WHERE target_kind='publish' AND received_at IS NULL;

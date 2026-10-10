-- Standalone measurement plans do not require enterprise knowledge or a cycle.
CREATE TABLE measurement_execution_plans (
    plan_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    idempotency_key TEXT NOT NULL CHECK (length(idempotency_key) > 0),
    request_hash TEXT NOT NULL CHECK (length(request_hash) > 0),
    input_hash TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    plan JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    UNIQUE (operator_id, tenant_id, project_id, plan_id),
    UNIQUE (operator_id, tenant_id, project_id, idempotency_key),
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT
);

ALTER TABLE channel_execution_targets ADD COLUMN measurement_plan_id UUID;
ALTER TABLE channel_execution_targets ALTER COLUMN cycle_id DROP NOT NULL;
ALTER TABLE channel_execution_targets DROP CONSTRAINT channel_target_exactly_one_owner;
ALTER TABLE channel_execution_targets ADD CONSTRAINT channel_target_exactly_one_owner
    CHECK (
        (plan_id IS NOT NULL AND publication_intent_id IS NULL
            AND measurement_plan_id IS NULL AND cycle_id IS NOT NULL AND ordinal IS NOT NULL)
        OR (plan_id IS NULL AND publication_intent_id IS NOT NULL
            AND measurement_plan_id IS NULL AND cycle_id IS NOT NULL
            AND ordinal IS NULL AND kind = 'publish')
        OR (plan_id IS NULL AND publication_intent_id IS NULL
            AND measurement_plan_id IS NOT NULL AND cycle_id IS NULL
            AND ordinal IS NOT NULL AND kind = 'measure')
    );
ALTER TABLE channel_execution_targets ADD CONSTRAINT channel_target_scoped_measurement_plan
    FOREIGN KEY (operator_id, tenant_id, project_id, measurement_plan_id)
    REFERENCES measurement_execution_plans (operator_id, tenant_id, project_id, plan_id)
    ON DELETE RESTRICT;
CREATE UNIQUE INDEX channel_target_measurement_ordinal_idx
    ON channel_execution_targets (operator_id, tenant_id, project_id, measurement_plan_id, ordinal)
    WHERE measurement_plan_id IS NOT NULL;

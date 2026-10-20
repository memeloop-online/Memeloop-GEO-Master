-- Manually requested content shares the existing manifest and execution model.
-- Its scope is a real project and never a fabricated optimization cycle.
ALTER TABLE document_manifests
    ALTER COLUMN cycle_id DROP NOT NULL,
    ADD COLUMN origin_kind TEXT NOT NULL DEFAULT 'cycle',
    ADD CONSTRAINT document_manifests_origin_check CHECK (
        (origin_kind='cycle' AND cycle_id IS NOT NULL)
        OR (origin_kind='on_demand' AND cycle_id IS NULL)
    );

ALTER TABLE content_executions
    ALTER COLUMN cycle_id DROP NOT NULL,
    ADD COLUMN origin_kind TEXT NOT NULL DEFAULT 'cycle',
    ADD COLUMN request_id UUID,
    ADD CONSTRAINT content_executions_project_fk
        FOREIGN KEY (operator_id,tenant_id,project_id)
        REFERENCES projects (operator_id,tenant_id,project_id) ON DELETE RESTRICT,
    ADD CONSTRAINT content_executions_origin_check CHECK (
        (origin_kind='cycle' AND cycle_id IS NOT NULL AND request_id IS NULL)
        OR (origin_kind='on_demand' AND cycle_id IS NULL AND request_id IS NOT NULL)
    );

CREATE UNIQUE INDEX content_executions_on_demand_request_unique
    ON content_executions (operator_id,tenant_id,project_id,request_id)
    WHERE origin_kind='on_demand';

-- Upgrade persisted aggregates once; runtime readers only accept the current
-- execution origin representation. Existing nonempty brand strings already
-- deserialize as the optional brand value without rewriting semantic hashes.
UPDATE content_executions
SET state=jsonb_set(state,'{execution}',
    ((state->'execution') - 'cycle_id') || jsonb_build_object(
        'origin',jsonb_build_object('kind','cycle','cycle_id',cycle_id)));

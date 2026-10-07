-- An intent has exactly one authoritative, scoped origin. Existing rows keep
-- their coverage target; a single-article request has no fabricated cycle.
ALTER TABLE distribution_publication_intents ALTER COLUMN origin_target_id DROP NOT NULL;
ALTER TABLE distribution_publication_intents ADD COLUMN origin_request_id UUID;
ALTER TABLE distribution_publication_intents ADD CONSTRAINT distribution_intent_exactly_one_origin
    CHECK ((origin_target_id IS NULL) <> (origin_request_id IS NULL));
ALTER TABLE distribution_publication_intents ADD CONSTRAINT distribution_intent_scoped_request
    FOREIGN KEY (operator_id, tenant_id, project_id, origin_request_id)
    REFERENCES content_distribution_requests (operator_id, tenant_id, project_id, request_id)
    ON DELETE RESTRICT;

ALTER TABLE distribution_publication_commands ALTER COLUMN origin_target_id DROP NOT NULL;
ALTER TABLE distribution_publication_commands ADD COLUMN origin_request_id UUID;
ALTER TABLE distribution_publication_commands ADD CONSTRAINT distribution_command_exactly_one_origin
    CHECK ((origin_target_id IS NULL) <> (origin_request_id IS NULL));
ALTER TABLE distribution_publication_commands ADD CONSTRAINT distribution_command_scoped_request
    FOREIGN KEY (operator_id, tenant_id, project_id, origin_request_id)
    REFERENCES content_distribution_requests (operator_id, tenant_id, project_id, request_id)
    ON DELETE RESTRICT;

-- Only generated-intent jobs may omit a cycle. Existing planned publication
-- and measurement ownership constraints retain their original cycle rules.
ALTER TABLE channel_execution_targets DROP CONSTRAINT channel_target_exactly_one_owner;
ALTER TABLE channel_execution_targets ADD CONSTRAINT channel_target_exactly_one_owner
    CHECK (
        (plan_id IS NOT NULL AND publication_intent_id IS NULL
            AND measurement_plan_id IS NULL AND cycle_id IS NOT NULL AND ordinal IS NOT NULL)
        OR (plan_id IS NULL AND publication_intent_id IS NOT NULL
            AND measurement_plan_id IS NULL AND ordinal IS NULL AND kind = 'publish')
        OR (plan_id IS NULL AND publication_intent_id IS NULL
            AND measurement_plan_id IS NOT NULL AND cycle_id IS NULL
            AND ordinal IS NOT NULL AND kind = 'measure')
    );

CREATE INDEX content_distribution_requests_unlinked_idx
    ON content_distribution_requests (request_id)
    WHERE publication_intent_id IS NULL;

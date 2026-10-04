-- Legacy frozen plans remain unique per cycle. A generated publication job
-- instead belongs to its immutable, project-scoped logical intent.
ALTER TABLE channel_execution_targets ALTER COLUMN plan_id DROP NOT NULL;
ALTER TABLE channel_execution_targets ALTER COLUMN ordinal DROP NOT NULL;
ALTER TABLE channel_execution_targets ADD COLUMN publication_intent_id UUID;
ALTER TABLE channel_execution_targets ADD CONSTRAINT channel_target_exactly_one_owner
    CHECK ((plan_id IS NOT NULL AND publication_intent_id IS NULL AND ordinal IS NOT NULL)
        OR (plan_id IS NULL AND publication_intent_id IS NOT NULL
            AND ordinal IS NULL AND kind = 'publish'));
ALTER TABLE channel_execution_targets ADD CONSTRAINT channel_target_scoped_publication_intent
    FOREIGN KEY (operator_id, tenant_id, project_id, publication_intent_id)
    REFERENCES distribution_publication_intents (operator_id, tenant_id, project_id, intent_id)
    ON DELETE RESTRICT;
CREATE UNIQUE INDEX channel_target_generated_intent_idx
    ON channel_execution_targets (operator_id, tenant_id, project_id, publication_intent_id)
    WHERE publication_intent_id IS NOT NULL;

-- The target and this marker are committed together. The marker never means
-- an external delivery, attempt, or verification happened.
ALTER TABLE distribution_publication_commands ADD COLUMN materialized_target_id UUID;
ALTER TABLE distribution_publication_commands ADD COLUMN materialized_at TIMESTAMPTZ;
ALTER TABLE distribution_publication_commands ADD CONSTRAINT distribution_command_materialized_pair
    CHECK ((materialized_target_id IS NULL) = (materialized_at IS NULL));
ALTER TABLE distribution_publication_commands ADD CONSTRAINT distribution_command_scoped_materialized_target
    FOREIGN KEY (operator_id, tenant_id, project_id, materialized_target_id)
    REFERENCES channel_execution_targets (operator_id, tenant_id, project_id, target_id)
    ON DELETE RESTRICT;
CREATE UNIQUE INDEX distribution_command_materialized_target_idx
    ON distribution_publication_commands (operator_id, tenant_id, project_id, materialized_target_id)
    WHERE materialized_target_id IS NOT NULL;
CREATE INDEX distribution_commands_bridge_pending_idx
    ON distribution_publication_commands (command_id)
    WHERE materialized_target_id IS NULL;

-- Saved publication proofs must retain scoped immutable source references.
-- Existing manually inserted DTO proofs remain separate with NULL saved IDs.
ALTER TABLE connector_capability_verifications
    ADD COLUMN tenant_id UUID,
    ADD COLUMN project_id UUID,
    ADD COLUMN target_id UUID,
    ADD COLUMN attempt_id UUID,
    ADD COLUMN account_id UUID,
    ADD COLUMN source_id UUID,
    ADD COLUMN source_version_id UUID,
    ADD COLUMN content_revision_id UUID,
    ADD COLUMN variant_id UUID,
    ADD COLUMN publication_intent_id UUID,
    ADD COLUMN body_sha256 TEXT,
    ADD COLUMN payload_hash TEXT,
    ADD COLUMN observed_at TIMESTAMPTZ,
    ADD COLUMN received_at TIMESTAMPTZ,
    ADD CONSTRAINT connector_saved_proof_complete CHECK (
        (attempt_id IS NULL AND tenant_id IS NULL AND project_id IS NULL AND target_id IS NULL
            AND account_id IS NULL AND source_id IS NULL AND source_version_id IS NULL
            AND content_revision_id IS NULL AND variant_id IS NULL AND publication_intent_id IS NULL
            AND body_sha256 IS NULL AND payload_hash IS NULL AND observed_at IS NULL AND received_at IS NULL)
        OR (attempt_id IS NOT NULL AND tenant_id IS NOT NULL AND project_id IS NOT NULL
            AND target_id IS NOT NULL AND account_id IS NOT NULL AND body_sha256 IS NOT NULL
            AND payload_hash IS NOT NULL AND observed_at IS NOT NULL AND received_at IS NOT NULL
            AND ((source_id IS NOT NULL AND source_version_id IS NOT NULL
                AND content_revision_id IS NULL AND variant_id IS NULL AND publication_intent_id IS NULL)
                OR (source_id IS NULL AND source_version_id IS NULL
                    AND content_revision_id IS NOT NULL AND variant_id IS NOT NULL
                    AND publication_intent_id IS NOT NULL))
            AND observed_at <= received_at)
    ),
    ADD CONSTRAINT connector_saved_attempt_scope FOREIGN KEY (operator_id,tenant_id,project_id,attempt_id)
        REFERENCES channel_execution_attempts(operator_id,tenant_id,project_id,attempt_id) ON DELETE RESTRICT,
    ADD CONSTRAINT connector_saved_target_scope FOREIGN KEY (operator_id,tenant_id,project_id,target_id)
        REFERENCES channel_execution_targets(operator_id,tenant_id,project_id,target_id) ON DELETE RESTRICT,
    ADD CONSTRAINT connector_saved_source_scope FOREIGN KEY (operator_id,tenant_id,project_id,source_id)
        REFERENCES knowledge_sources(operator_id,tenant_id,project_id,source_id) ON DELETE RESTRICT,
    ADD CONSTRAINT connector_saved_version_scope FOREIGN KEY (operator_id,tenant_id,project_id,source_version_id)
        REFERENCES knowledge_source_versions(operator_id,tenant_id,project_id,source_version_id) ON DELETE RESTRICT,
    ADD CONSTRAINT connector_saved_revision_scope FOREIGN KEY (operator_id,tenant_id,project_id,content_revision_id)
        REFERENCES content_revisions(operator_id,tenant_id,project_id,revision_id) ON DELETE RESTRICT,
    ADD CONSTRAINT connector_saved_variant_scope FOREIGN KEY (operator_id,tenant_id,project_id,variant_id)
        REFERENCES distribution_channel_variants(operator_id,tenant_id,project_id,variant_id) ON DELETE RESTRICT,
    ADD CONSTRAINT connector_saved_intent_scope FOREIGN KEY (operator_id,tenant_id,project_id,publication_intent_id)
        REFERENCES distribution_publication_intents(operator_id,tenant_id,project_id,intent_id) ON DELETE RESTRICT;
CREATE UNIQUE INDEX connector_saved_attempt_format_unique
    ON connector_capability_verifications(operator_id,attempt_id,content_type)
    WHERE attempt_id IS NOT NULL;
CREATE INDEX connector_saved_attempt_scan_idx
    ON channel_execution_attempts(attempt_id)
    WHERE outcome IS NOT NULL AND target_kind='publish';

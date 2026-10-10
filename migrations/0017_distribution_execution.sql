-- The project-start distribution manifest is an unsealed skeleton referenced
-- by start records. A separate immutable execution revision freezes handoff
-- and platform coverage without changing the original acceptance identity.
ALTER TABLE content_handoffs ADD CONSTRAINT content_handoffs_scoped_id
    UNIQUE (operator_id, tenant_id, project_id, handoff_id);
ALTER TABLE content_revisions ADD CONSTRAINT content_revisions_scoped_id
    UNIQUE (operator_id, tenant_id, project_id, revision_id);
CREATE TABLE distribution_execution_manifests (
    manifest_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    cycle_id UUID NOT NULL,
    skeleton_manifest_id UUID NOT NULL,
    document_manifest_id UUID NOT NULL,
    content_execution_id UUID NOT NULL,
    content_handoff_id UUID NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    input_hash TEXT NOT NULL,
    frozen JSONB NOT NULL,
    expected_count BIGINT NOT NULL CHECK (expected_count >= 0),
    expansion_cursor BIGINT NOT NULL DEFAULT 0 CHECK (expansion_cursor >= 0),
    complete BOOLEAN NOT NULL DEFAULT false,
    sealed_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (expansion_cursor <= expected_count),
    CHECK (complete = (expansion_cursor = expected_count)),
    UNIQUE (operator_id, tenant_id, project_id, manifest_id),
    UNIQUE (operator_id, tenant_id, project_id, cycle_id, revision),
    FOREIGN KEY (operator_id, tenant_id, project_id, skeleton_manifest_id)
        REFERENCES distribution_manifests (operator_id, tenant_id, project_id, manifest_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, document_manifest_id)
        REFERENCES document_manifests (operator_id, tenant_id, project_id, manifest_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, content_execution_id)
        REFERENCES content_executions (operator_id, tenant_id, project_id, execution_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, content_handoff_id)
        REFERENCES content_handoffs (operator_id, tenant_id, project_id, handoff_id) ON DELETE RESTRICT
);
CREATE INDEX distribution_execution_cycle_idx ON distribution_execution_manifests
    (operator_id, tenant_id, project_id, cycle_id, revision DESC);

CREATE TABLE distribution_execution_targets (
    target_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    manifest_id UUID NOT NULL,
    ordinal BIGINT NOT NULL CHECK (ordinal >= 0),
    current_version BIGINT NOT NULL CHECK (current_version > 0),
    current_body JSONB NOT NULL,
    UNIQUE (operator_id, tenant_id, project_id, target_id),
    UNIQUE (operator_id, tenant_id, project_id, manifest_id, ordinal),
    FOREIGN KEY (operator_id, tenant_id, project_id, manifest_id)
        REFERENCES distribution_execution_manifests (operator_id, tenant_id, project_id, manifest_id) ON DELETE RESTRICT
);
CREATE INDEX distribution_execution_target_page_idx ON distribution_execution_targets
    (operator_id, tenant_id, project_id, manifest_id, ordinal);
CREATE TABLE distribution_target_versions (
    target_id UUID NOT NULL,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    version BIGINT NOT NULL CHECK (version > 0),
    body JSONB NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (target_id, version),
    FOREIGN KEY (operator_id, tenant_id, project_id, target_id)
        REFERENCES distribution_execution_targets (operator_id, tenant_id, project_id, target_id) ON DELETE RESTRICT
);
CREATE INDEX distribution_target_versions_cutoff_idx ON distribution_target_versions
    (operator_id, tenant_id, project_id, recorded_at, target_id, version DESC);

CREATE TABLE distribution_channel_variants (
    variant_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    content_revision_id UUID NOT NULL,
    body JSONB NOT NULL,
    UNIQUE (operator_id, tenant_id, project_id, variant_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, content_revision_id)
        REFERENCES content_revisions (operator_id, tenant_id, project_id, revision_id) ON DELETE RESTRICT
);
CREATE TABLE distribution_publication_intents (
    intent_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    origin_target_id UUID NOT NULL,
    variant_id UUID NOT NULL,
    logical_key TEXT NOT NULL,
    verification TEXT NOT NULL DEFAULT 'unverified'
        CHECK (verification IN ('unverified', 'unknown', 'verified')),
    verification_evidence_id UUID,
    body JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (operator_id, tenant_id, project_id, intent_id),
    UNIQUE (operator_id, tenant_id, project_id, logical_key),
    CHECK ((verification = 'unverified') = (verification_evidence_id IS NULL)),
    FOREIGN KEY (operator_id, tenant_id, project_id, origin_target_id)
        REFERENCES distribution_execution_targets (operator_id, tenant_id, project_id, target_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, variant_id)
        REFERENCES distribution_channel_variants (operator_id, tenant_id, project_id, variant_id) ON DELETE RESTRICT
);
-- The outbox is created with the intent in the same transaction. It does not
-- imply delivery or successful external publication.
CREATE TABLE distribution_publication_commands (
    command_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    intent_id UUID NOT NULL,
    origin_target_id UUID NOT NULL,
    payload_hash TEXT NOT NULL,
    fixture BOOLEAN NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'claimed', 'delivered')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (operator_id, tenant_id, project_id, intent_id),
    UNIQUE (operator_id, tenant_id, project_id, intent_id, command_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, intent_id)
        REFERENCES distribution_publication_intents (operator_id, tenant_id, project_id, intent_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, origin_target_id)
        REFERENCES distribution_execution_targets (operator_id, tenant_id, project_id, target_id) ON DELETE RESTRICT
);
CREATE TABLE distribution_publication_attempts (
    attempt_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    intent_id UUID NOT NULL,
    command_id UUID NOT NULL,
    claimed_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (operator_id, tenant_id, project_id, attempt_id),
    UNIQUE (operator_id, tenant_id, project_id, intent_id),
    UNIQUE (operator_id, tenant_id, project_id, intent_id, attempt_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, intent_id)
        REFERENCES distribution_publication_intents (operator_id, tenant_id, project_id, intent_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, intent_id, command_id)
        REFERENCES distribution_publication_commands
        (operator_id, tenant_id, project_id, intent_id, command_id) ON DELETE RESTRICT
);
-- Only a trusted receipt/readback adapter may populate evidence. Merely
-- knowing a UUID never upgrades an unknown external result to verified.
CREATE TABLE distribution_intent_evidence (
    evidence_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    intent_id UUID NOT NULL,
    result TEXT NOT NULL CHECK (result IN ('unknown', 'verified')),
    attempt_id UUID NOT NULL,
    external_receipt JSONB,
    public_readback JSONB,
    fixture BOOLEAN NOT NULL DEFAULT false,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (result <> 'verified' OR
        (fixture = false
         AND external_receipt IS NOT NULL
         AND jsonb_typeof(external_receipt) = 'object'
         AND external_receipt ? 'external_receipt_id'
         AND public_readback IS NOT NULL
         AND jsonb_typeof(public_readback) = 'object'
         AND public_readback ? 'public_url'
         AND public_readback->>'verified' = 'true')),
    FOREIGN KEY (operator_id, tenant_id, project_id, intent_id, attempt_id)
        REFERENCES distribution_publication_attempts
        (operator_id, tenant_id, project_id, intent_id, attempt_id) ON DELETE RESTRICT
);
CREATE FUNCTION reject_distribution_immutable_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'frozen distribution input or evidence is immutable';
END $$;
CREATE TRIGGER distribution_versions_immutable BEFORE UPDATE OR DELETE
    ON distribution_target_versions FOR EACH ROW EXECUTE FUNCTION reject_distribution_immutable_change();
CREATE TRIGGER distribution_variants_immutable BEFORE UPDATE OR DELETE
    ON distribution_channel_variants FOR EACH ROW EXECUTE FUNCTION reject_distribution_immutable_change();
CREATE TRIGGER distribution_evidence_immutable BEFORE UPDATE OR DELETE
    ON distribution_intent_evidence FOR EACH ROW EXECUTE FUNCTION reject_distribution_immutable_change();
CREATE TRIGGER distribution_attempts_immutable BEFORE UPDATE OR DELETE
    ON distribution_publication_attempts FOR EACH ROW EXECUTE FUNCTION reject_distribution_immutable_change();
CREATE FUNCTION guard_distribution_frozen_manifest() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.manifest_id IS DISTINCT FROM OLD.manifest_id
       OR NEW.operator_id IS DISTINCT FROM OLD.operator_id
       OR NEW.tenant_id IS DISTINCT FROM OLD.tenant_id
       OR NEW.project_id IS DISTINCT FROM OLD.project_id
       OR NEW.cycle_id IS DISTINCT FROM OLD.cycle_id
       OR NEW.skeleton_manifest_id IS DISTINCT FROM OLD.skeleton_manifest_id
       OR NEW.document_manifest_id IS DISTINCT FROM OLD.document_manifest_id
       OR NEW.content_execution_id IS DISTINCT FROM OLD.content_execution_id
       OR NEW.content_handoff_id IS DISTINCT FROM OLD.content_handoff_id
       OR NEW.revision IS DISTINCT FROM OLD.revision
       OR NEW.input_hash IS DISTINCT FROM OLD.input_hash
       OR NEW.frozen IS DISTINCT FROM OLD.frozen
       OR NEW.expected_count IS DISTINCT FROM OLD.expected_count
       OR NEW.sealed_at IS DISTINCT FROM OLD.sealed_at
       OR NEW.expansion_cursor < OLD.expansion_cursor
    THEN RAISE EXCEPTION 'distribution manifest frozen input cannot be rewritten';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER distribution_frozen_manifest_guard BEFORE UPDATE
    ON distribution_execution_manifests FOR EACH ROW EXECUTE FUNCTION guard_distribution_frozen_manifest();

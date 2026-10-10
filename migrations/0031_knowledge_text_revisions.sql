-- Authored bodies are immutable and tied to the precise version AND source.
ALTER TABLE knowledge_source_versions
    ADD COLUMN representation TEXT NOT NULL DEFAULT 'original'
    CHECK (representation IN ('original', 'authored_text'));
ALTER TABLE knowledge_source_versions
    ADD CONSTRAINT knowledge_source_versions_source_identity_unique
    UNIQUE (operator_id, tenant_id, project_id, source_id, source_version_id);

CREATE TABLE knowledge_authored_text (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    source_id UUID NOT NULL,
    source_version_id UUID NOT NULL,
    media_type TEXT NOT NULL CHECK (media_type IN ('text/plain', 'text/markdown')),
    body TEXT NOT NULL CHECK (length(btrim(body)) > 0 AND octet_length(body) <= 262144),
    PRIMARY KEY (operator_id, tenant_id, project_id, source_version_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, source_id, source_version_id)
        REFERENCES knowledge_source_versions
            (operator_id, tenant_id, project_id, source_id, source_version_id)
        ON DELETE RESTRICT
);

ALTER TABLE knowledge_import_receipts DROP CONSTRAINT knowledge_import_receipts_action_check;
ALTER TABLE knowledge_import_receipts ADD CONSTRAINT knowledge_import_receipts_action_check
    CHECK (action IN ('import_item', 'upload_complete', 'source_text_revision'));
-- Old receipts still have exactly one target per upload; revisions need
-- multiple independent keys on one source and immutable replay across edits.
DO $$
DECLARE old_unique name;
BEGIN
    SELECT conname INTO old_unique
    FROM pg_constraint
    WHERE conrelid = 'knowledge_import_receipts'::regclass
      AND contype = 'u'
      AND pg_get_constraintdef(oid) =
          'UNIQUE (operator_id, tenant_id, project_id, action, target_id)';
    IF old_unique IS NULL THEN
        RAISE EXCEPTION 'knowledge receipt target constraint was not found';
    END IF;
    EXECUTE format('ALTER TABLE knowledge_import_receipts DROP CONSTRAINT %I', old_unique);
END
$$;
CREATE UNIQUE INDEX knowledge_import_receipts_single_target_idx
    ON knowledge_import_receipts (operator_id, tenant_id, project_id, action, target_id)
    WHERE action IN ('import_item', 'upload_complete');
CREATE UNIQUE INDEX knowledge_import_receipts_revision_key_idx
    ON knowledge_import_receipts
        (operator_id, tenant_id, project_id, action, target_id, client_item_id)
    WHERE action = 'source_text_revision';

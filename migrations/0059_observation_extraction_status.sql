-- Target/attempt status reads enter through the indexed immutable captures and
-- inspect only the tasks bound to those captures, without loading model text.
CREATE INDEX observation_extraction_capture_status_idx
    ON observation_extraction_intents
        (operator_id, tenant_id, project_id, source_capture_id, created_at DESC, intent_id DESC);

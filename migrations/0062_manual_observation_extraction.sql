-- Existing automatic identities and JSON remain unchanged. Manual generations
-- reference the original user-visible revision, including evidence-only sources.
ALTER TABLE observation_analyses ADD CONSTRAINT observation_analyses_scoped_revision
    UNIQUE (operator_id,tenant_id,project_id,revision_id);
ALTER TABLE observation_extraction_intents
    ALTER COLUMN source_capture_id DROP NOT NULL,
    ADD COLUMN source_evidence_index BIGINT,
    ADD COLUMN manual_revision_id UUID,
    ADD CONSTRAINT observation_extraction_manual_unique
        UNIQUE (operator_id,tenant_id,project_id,manual_revision_id),
    ADD CONSTRAINT observation_extraction_manual_fk
        FOREIGN KEY (operator_id,tenant_id,project_id,manual_revision_id)
        REFERENCES observation_analyses (operator_id,tenant_id,project_id,revision_id) ON DELETE RESTRICT,
    ADD CONSTRAINT observation_extraction_source_xor
        CHECK ((source_capture_id IS NOT NULL) <> (source_evidence_index IS NOT NULL)),
    ADD CONSTRAINT observation_extraction_evidence_range
        CHECK (source_evidence_index IS NULL OR
            (manual_revision_id IS NOT NULL AND source_evidence_index BETWEEN 0 AND 4294967295)),
    ADD CONSTRAINT observation_extraction_json_binding CHECK (
        source_capture_id IS NOT DISTINCT FROM (intent->'request'->>'source_capture_id')::uuid
        AND source_evidence_index IS NOT DISTINCT FROM (intent->'request'->>'source_evidence_index')::bigint
        AND manual_revision_id IS NOT DISTINCT FROM (intent->'request'->>'manual_revision_id')::uuid
        AND state IS NOT DISTINCT FROM intent->>'state'
        AND intent_id IS NOT DISTINCT FROM (intent->>'intent_id')::uuid
        AND intent_key::text IS NOT DISTINCT FROM intent->>'intent_key'
        AND (intent->'request'->>'target_id') IS NOT NULL
        AND (intent->'request'->>'attempt_id') IS NOT NULL
    );

CREATE OR REPLACE FUNCTION guard_observation_extraction_intent_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN RAISE EXCEPTION 'extraction intents cannot be deleted'; END IF;
    IF OLD.state IN ('completed','interrupted') THEN
        RAISE EXCEPTION 'terminal extraction intents are immutable';
    END IF;
    IF NEW.operator_id IS DISTINCT FROM OLD.operator_id OR NEW.tenant_id IS DISTINCT FROM OLD.tenant_id
       OR NEW.project_id IS DISTINCT FROM OLD.project_id OR NEW.intent_id IS DISTINCT FROM OLD.intent_id
       OR NEW.source_capture_id IS DISTINCT FROM OLD.source_capture_id
       OR NEW.source_evidence_index IS DISTINCT FROM OLD.source_evidence_index
       OR NEW.manual_revision_id IS DISTINCT FROM OLD.manual_revision_id
       OR NEW.intent_key IS DISTINCT FROM OLD.intent_key OR NEW.created_at IS DISTINCT FROM OLD.created_at
       OR NEW.intent->'request' IS DISTINCT FROM OLD.intent->'request' THEN
        RAISE EXCEPTION 'extraction binding is immutable';
    END IF;
    IF OLD.intent->'response' IS NOT NULL AND OLD.intent->'response' <> 'null'::jsonb
       AND NEW.intent->'response' IS DISTINCT FROM OLD.intent->'response' THEN
        RAISE EXCEPTION 'extraction responses are immutable';
    END IF;
    IF NOT ((OLD.state='queued' AND NEW.state='running')
        OR (OLD.state='running' AND NEW.state IN ('response_received','interrupted'))
        OR (OLD.state='response_received' AND NEW.state IN ('completed','interrupted'))) THEN
        RAISE EXCEPTION 'invalid extraction transition';
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION guard_manual_observation_extraction_insert() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE analysis_request JSONB;
BEGIN
    IF NEW.manual_revision_id IS NULL THEN RETURN NEW; END IF;
    SELECT request INTO analysis_request FROM observation_analyses
        WHERE operator_id=NEW.operator_id AND tenant_id=NEW.tenant_id
        AND project_id=NEW.project_id AND revision_id=NEW.manual_revision_id
        AND state='queued' AND created_at=NEW.created_at;
    IF analysis_request IS NULL
       OR analysis_request->>'target_id' IS DISTINCT FROM NEW.intent->'request'->>'target_id'
       OR analysis_request->>'attempt_id' IS DISTINCT FROM NEW.intent->'request'->>'attempt_id'
       OR analysis_request->>'source_sha256' IS DISTINCT FROM NEW.intent->'request'->>'source_sha256'
       OR analysis_request->>'prompt_version' IS DISTINCT FROM NEW.intent->'request'->>'prompt_version'
       OR analysis_request->>'parser_version' IS DISTINCT FROM NEW.intent->'request'->>'parser_version'
       OR (NEW.source_capture_id IS NOT NULL AND
           ((analysis_request->'source'->>'capture_id')::uuid IS DISTINCT FROM NEW.source_capture_id
            OR analysis_request->'source'->>'kind' IS DISTINCT FROM 'capture'))
       OR (NEW.source_evidence_index IS NOT NULL AND
           ((analysis_request->'source'->>'evidence_index')::bigint IS DISTINCT FROM NEW.source_evidence_index
            OR analysis_request->'source'->>'kind' IS DISTINCT FROM 'attempt_evidence'))
    THEN RAISE EXCEPTION 'manual extraction analysis binding differs'; END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER observation_extraction_manual_insert BEFORE INSERT ON observation_extraction_intents
    FOR EACH ROW EXECUTE FUNCTION guard_manual_observation_extraction_insert();
CREATE INDEX observation_extraction_attempt_status ON observation_extraction_intents
    (operator_id,tenant_id,project_id,(intent->'request'->>'target_id'),(intent->'request'->>'attempt_id'),created_at DESC);

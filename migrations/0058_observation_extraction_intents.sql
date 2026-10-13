-- One automatic inference per scoped capture and versioned extraction policy.
-- Received model text remains durable even after interruption or parse failure.
CREATE TABLE observation_extraction_intents (
    intent_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    source_capture_id UUID NOT NULL,
    intent_key CHAR(64) NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('queued','running','response_received','completed','interrupted')),
    created_at TIMESTAMPTZ NOT NULL,
    claim_token UUID,
    intent JSONB NOT NULL,
    UNIQUE (operator_id,tenant_id,project_id,intent_key),
    FOREIGN KEY (operator_id,tenant_id,project_id,source_capture_id)
        REFERENCES observation_captures (operator_id,tenant_id,project_id,capture_id) ON DELETE RESTRICT,
    CHECK (state = intent->>'state'),
    CHECK (intent_id = (intent->>'intent_id')::uuid),
    CHECK (intent_key = intent->>'intent_key'),
    CHECK (source_capture_id = (intent->'request'->>'source_capture_id')::uuid),
    CHECK ((state = 'queued' AND claim_token IS NULL) OR (state <> 'queued' AND claim_token IS NOT NULL)),
    -- A control character may require six JSON bytes per source UTF-8 byte.
    CHECK (octet_length(intent::text) <= 14000000),
    CHECK (octet_length(intent->'response'->>'model_text') <= 2000000)
);
CREATE INDEX observation_extraction_ready_idx ON observation_extraction_intents (intent_id,created_at)
    WHERE state IN ('queued','response_received');

-- Write-once responses survive all local parsing outcomes. A terminal row is
-- immutable; an interrupted running inference cannot be implicitly requeued.
CREATE FUNCTION guard_observation_extraction_intent_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN RAISE EXCEPTION 'extraction intents cannot be deleted'; END IF;
    IF OLD.state IN ('completed','interrupted') THEN
        RAISE EXCEPTION 'terminal extraction intents are immutable';
    END IF;
    IF NEW.operator_id <> OLD.operator_id OR NEW.tenant_id <> OLD.tenant_id
       OR NEW.project_id <> OLD.project_id OR NEW.intent_id <> OLD.intent_id
       OR NEW.source_capture_id <> OLD.source_capture_id OR NEW.intent_key <> OLD.intent_key
       OR NEW.created_at <> OLD.created_at
       OR NEW.intent->'request' IS DISTINCT FROM OLD.intent->'request' THEN
        RAISE EXCEPTION 'extraction binding is immutable';
    END IF;
    IF OLD.intent->'response' <> 'null'::jsonb
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
CREATE TRIGGER observation_extraction_intents_guard BEFORE UPDATE OR DELETE ON observation_extraction_intents
    FOR EACH ROW EXECUTE FUNCTION guard_observation_extraction_intent_change();

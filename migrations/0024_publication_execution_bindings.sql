-- Opaque encrypted, write-once execution context for the original send.
-- Only the server-side channel repository reads these bytes; never project
-- them into target/outcome JSON or public lookup observations.
ALTER TABLE channel_execution_targets
    ADD CONSTRAINT channel_target_scoped_kind_unique
    UNIQUE (operator_id, tenant_id, project_id, target_id, kind);
ALTER TABLE channel_execution_attempts
    ADD CONSTRAINT channel_attempt_scoped_target_kind_unique
    UNIQUE (operator_id, tenant_id, project_id, target_id, attempt_id, target_kind);

CREATE TABLE publication_execution_bindings (
    attempt_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    target_id UUID NOT NULL,
    target_kind TEXT NOT NULL DEFAULT 'publish' CHECK (target_kind = 'publish'),
    encrypted_binding BYTEA NOT NULL CHECK (octet_length(encrypted_binding) > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id, project_id, target_id, attempt_id, target_kind)
        REFERENCES channel_execution_attempts
        (operator_id, tenant_id, project_id, target_id, attempt_id, target_kind)
        ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, target_id, target_kind)
        REFERENCES channel_execution_targets
        (operator_id, tenant_id, project_id, target_id, kind)
        ON DELETE RESTRICT
);

CREATE FUNCTION publication_binding_before_insert() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    original_outcome JSONB;
BEGIN
    -- Serialize with finish(), which updates this same attempt row. An
    -- insert after finish is never interpreted as pre-send provenance.
    SELECT outcome INTO original_outcome
      FROM channel_execution_attempts
     WHERE operator_id = NEW.operator_id
       AND tenant_id = NEW.tenant_id
       AND project_id = NEW.project_id
       AND target_id = NEW.target_id
       AND attempt_id = NEW.attempt_id
       AND target_kind = 'publish'
     FOR UPDATE;
    IF NOT FOUND OR original_outcome IS NOT NULL THEN
        RAISE EXCEPTION 'publication binding requires an unfinished send attempt';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER publication_binding_pre_send
    BEFORE INSERT ON publication_execution_bindings
    FOR EACH ROW EXECUTE FUNCTION publication_binding_before_insert();

CREATE FUNCTION publication_binding_prevent_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'publication execution binding is immutable';
END
$$;
CREATE TRIGGER publication_binding_immutable
    BEFORE UPDATE OR DELETE ON publication_execution_bindings
    FOR EACH ROW EXECUTE FUNCTION publication_binding_prevent_mutation();

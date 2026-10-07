-- A media use grant is project-local and tied to one immutable object identity.
-- A withdrawn grant cannot be revived, including through direct SQL.
CREATE TABLE content_media_bindings (
    binding_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    object_id UUID NOT NULL,
    object_version BIGINT NOT NULL CHECK (object_version > 0),
    sha256 TEXT NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    media_type TEXT NOT NULL CHECK (media_type IN ('image/png','image/jpeg','image/webp')),
    byte_len BIGINT NOT NULL CHECK (byte_len > 0 AND byte_len <= 104857600),
    width INTEGER NOT NULL CHECK (width BETWEEN 1 AND 16384),
    height INTEGER NOT NULL CHECK (height BETWEEN 1 AND 16384),
    state TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active','withdrawn')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    withdrawn_at TIMESTAMPTZ,
    FOREIGN KEY (operator_id,tenant_id,project_id)
        REFERENCES projects (operator_id,tenant_id,project_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id,tenant_id,project_id,object_id)
        REFERENCES knowledge_stored_objects (operator_id,tenant_id,project_id,object_id) ON DELETE RESTRICT,
    UNIQUE (operator_id,tenant_id,project_id,binding_id),
    UNIQUE (operator_id,tenant_id,project_id,object_id,object_version,sha256),
    CHECK ((state='active' AND withdrawn_at IS NULL)
        OR (state='withdrawn' AND withdrawn_at IS NOT NULL)),
    CHECK (width::bigint * height::bigint <= 100000000)
);
CREATE INDEX content_media_bindings_active_scope_idx
    ON content_media_bindings (operator_id,tenant_id,project_id,binding_id)
    WHERE state='active';
-- The committed attachment lookup must not scan every upload in a project.
CREATE INDEX knowledge_upload_sessions_agent_committed_object_idx
    ON knowledge_upload_sessions (operator_id,tenant_id,project_id,committed_object_id)
    WHERE state='committed' AND staging_object_ref='agent-attachment'
      AND committed_object_id IS NOT NULL;

CREATE FUNCTION content_media_binding_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='INSERT' THEN
        IF NEW.state<>'active' OR NEW.withdrawn_at IS NOT NULL
           OR NOT EXISTS (
               SELECT 1 FROM knowledge_stored_objects object
               JOIN knowledge_upload_sessions session
                 ON session.operator_id=object.operator_id
                AND session.tenant_id=object.tenant_id
                AND session.project_id=object.project_id
                AND session.committed_object_id=object.object_id
               WHERE object.operator_id=NEW.operator_id AND object.tenant_id=NEW.tenant_id
                 AND object.project_id=NEW.project_id AND object.object_id=NEW.object_id
                 AND object.object_version=NEW.object_version AND object.sha256=NEW.sha256
                 AND object.actual_size=NEW.byte_len AND object.state='committed'
                 AND object.backend='postgres_blob'
                 AND object.opaque_key='upload/' || session.upload_session_id::text
                 AND session.state='committed'
                 AND session.staging_object_ref='agent-attachment'
                 AND session.expected_sha256=NEW.sha256
                 AND session.expected_size=NEW.byte_len
           )
        THEN
            RAISE EXCEPTION 'media binding requires the committed scoped Agent attachment';
        END IF;
        RETURN NEW;
    END IF;
    IF TG_OP='DELETE' THEN
        RAISE EXCEPTION 'media grants cannot be deleted; withdraw them';
    END IF;
    IF NEW.binding_id IS DISTINCT FROM OLD.binding_id
        OR NEW.operator_id IS DISTINCT FROM OLD.operator_id
        OR NEW.tenant_id IS DISTINCT FROM OLD.tenant_id
        OR NEW.project_id IS DISTINCT FROM OLD.project_id
        OR NEW.object_id IS DISTINCT FROM OLD.object_id
        OR NEW.object_version IS DISTINCT FROM OLD.object_version
        OR NEW.sha256 IS DISTINCT FROM OLD.sha256
        OR NEW.media_type IS DISTINCT FROM OLD.media_type
        OR NEW.byte_len IS DISTINCT FROM OLD.byte_len
        OR NEW.width IS DISTINCT FROM OLD.width
        OR NEW.height IS DISTINCT FROM OLD.height
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
        OR OLD.state='withdrawn'
        OR OLD.state<>'active'
        OR NEW.state<>'withdrawn'
        OR NEW.withdrawn_at IS NULL
    THEN
        RAISE EXCEPTION 'media binding identity and withdrawal are immutable';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER content_media_binding_no_rewrite
    BEFORE INSERT OR UPDATE OR DELETE ON content_media_bindings
    FOR EACH ROW EXECUTE FUNCTION content_media_binding_guard();

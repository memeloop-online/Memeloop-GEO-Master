-- Project-scoped question identity, immutable set versions, and fixed split.
-- Scoped composite keys prevent a version, identity, revision, or alias from
-- being attached to another tenant or project even if a UUID is supplied.
CREATE TABLE question_project_registries (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    split_seed UUID NOT NULL,
    registered_count BIGINT NOT NULL DEFAULT 0 CHECK (registered_count >= 0),
    PRIMARY KEY (operator_id, tenant_id, project_id),
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE CASCADE
);

CREATE TABLE question_sets (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    question_set_id UUID NOT NULL,
    name TEXT NOT NULL,
    latest_version_id UUID,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (operator_id, tenant_id, project_id, question_set_id),
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES question_project_registries (operator_id, tenant_id, project_id)
        ON DELETE CASCADE
);

CREATE TABLE question_identities (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    question_id UUID NOT NULL,
    evaluation_split TEXT NOT NULL CHECK (evaluation_split IN ('optimization', 'frozen_evaluation')),
    split_policy_version TEXT NOT NULL,
    registered_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (operator_id, tenant_id, project_id, question_id),
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES question_project_registries (operator_id, tenant_id, project_id)
        ON DELETE CASCADE
);

CREATE TABLE question_identity_aliases (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    -- Fixed-length key avoids PostgreSQL btree tuple limits for valid 2,000-
    -- character Unicode questions. The original text is retained and checked
    -- on lookup; an improbable digest collision fails closed on insertion.
    normalized_digest TEXT NOT NULL CHECK (normalized_digest ~ '^[0-9a-f]{64}$'),
    normalized_text TEXT NOT NULL,
    question_id UUID NOT NULL,
    PRIMARY KEY (operator_id, tenant_id, project_id, normalized_digest),
    FOREIGN KEY (operator_id, tenant_id, project_id, question_id)
        REFERENCES question_identities (operator_id, tenant_id, project_id, question_id)
        ON DELETE CASCADE
);
CREATE INDEX question_identity_aliases_identity_idx
    ON question_identity_aliases (operator_id, tenant_id, project_id, question_id);

CREATE TABLE question_revisions (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    question_id UUID NOT NULL,
    question_revision_id UUID NOT NULL,
    revision_json JSONB NOT NULL,
    PRIMARY KEY (operator_id, tenant_id, project_id, question_id, question_revision_id),
    UNIQUE (operator_id, tenant_id, project_id, question_revision_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, question_id)
        REFERENCES question_identities (operator_id, tenant_id, project_id, question_id)
        ON DELETE RESTRICT
);

CREATE TABLE question_set_versions (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    question_set_id UUID NOT NULL,
    question_set_version_id UUID NOT NULL,
    revision BIGINT NOT NULL CHECK (revision > 0),
    name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    version_json JSONB NOT NULL,
    PRIMARY KEY (operator_id, tenant_id, project_id, question_set_id, question_set_version_id),
    UNIQUE (operator_id, tenant_id, project_id, question_set_version_id),
    UNIQUE (operator_id, tenant_id, project_id, question_set_id, revision),
    FOREIGN KEY (operator_id, tenant_id, project_id, question_set_id)
        REFERENCES question_sets (operator_id, tenant_id, project_id, question_set_id)
        ON DELETE RESTRICT
);
CREATE INDEX question_set_versions_history_idx
    ON question_set_versions (operator_id, tenant_id, project_id, question_set_id, revision DESC);

ALTER TABLE question_sets ADD CONSTRAINT question_sets_latest_version_fk
    FOREIGN KEY (operator_id, tenant_id, project_id, question_set_id, latest_version_id)
    REFERENCES question_set_versions (
        operator_id, tenant_id, project_id, question_set_id, question_set_version_id
    ) ON DELETE RESTRICT;

CREATE TABLE question_version_members (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    question_set_id UUID NOT NULL,
    question_set_version_id UUID NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 0),
    question_id UUID NOT NULL,
    question_revision_id UUID NOT NULL,
    PRIMARY KEY (operator_id, tenant_id, project_id, question_set_version_id, question_id),
    UNIQUE (operator_id, tenant_id, project_id, question_set_version_id, position),
    FOREIGN KEY (operator_id, tenant_id, project_id, question_set_id, question_set_version_id)
        REFERENCES question_set_versions (
            operator_id, tenant_id, project_id, question_set_id, question_set_version_id
        ) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, question_id, question_revision_id)
        REFERENCES question_revisions (
            operator_id, tenant_id, project_id, question_id, question_revision_id
        ) ON DELETE RESTRICT
);
CREATE INDEX question_version_members_identity_idx
    ON question_version_members (operator_id, tenant_id, project_id, question_id);

CREATE TABLE question_set_requests (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    question_set_id UUID NOT NULL,
    question_set_version_id UUID NOT NULL,
    PRIMARY KEY (operator_id, tenant_id, project_id, idempotency_key),
    FOREIGN KEY (operator_id, tenant_id, project_id, question_set_id, question_set_version_id)
        REFERENCES question_set_versions (
            operator_id, tenant_id, project_id, question_set_id, question_set_version_id
        ) ON DELETE RESTRICT
);

-- The application only appends registry identities, revisions, memberships,
-- aliases and versions. Guard that promise even if a future repository writer
-- accidentally attempts to rewrite a historical split or frozen response.
CREATE FUNCTION reject_question_history_change() RETURNS TRIGGER
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'published question history is immutable' USING ERRCODE = '23514';
END;
$$;

CREATE TRIGGER question_identities_immutable
    BEFORE UPDATE OR DELETE ON question_identities
    FOR EACH ROW EXECUTE FUNCTION reject_question_history_change();
CREATE TRIGGER question_identity_aliases_immutable
    BEFORE UPDATE OR DELETE ON question_identity_aliases
    FOR EACH ROW EXECUTE FUNCTION reject_question_history_change();
CREATE TRIGGER question_revisions_immutable
    BEFORE UPDATE OR DELETE ON question_revisions
    FOR EACH ROW EXECUTE FUNCTION reject_question_history_change();
CREATE TRIGGER question_set_versions_immutable
    BEFORE UPDATE OR DELETE ON question_set_versions
    FOR EACH ROW EXECUTE FUNCTION reject_question_history_change();
CREATE TRIGGER question_version_members_immutable
    BEFORE UPDATE OR DELETE ON question_version_members
    FOR EACH ROW EXECUTE FUNCTION reject_question_history_change();
CREATE TRIGGER question_set_requests_immutable
    BEFORE UPDATE OR DELETE ON question_set_requests
    FOR EACH ROW EXECUTE FUNCTION reject_question_history_change();

CREATE FUNCTION guard_question_registry_update() RETURNS TRIGGER
LANGUAGE plpgsql AS $$
BEGIN
    IF (NEW.operator_id, NEW.tenant_id, NEW.project_id, NEW.split_seed)
       IS DISTINCT FROM
       (OLD.operator_id, OLD.tenant_id, OLD.project_id, OLD.split_seed)
       OR NEW.registered_count < OLD.registered_count THEN
        RAISE EXCEPTION 'question registry scope, seed and enrollment are immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER question_project_registries_guard
    BEFORE UPDATE ON question_project_registries
    FOR EACH ROW EXECUTE FUNCTION guard_question_registry_update();

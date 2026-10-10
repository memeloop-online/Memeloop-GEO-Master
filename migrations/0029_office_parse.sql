-- Office document attempts retain a typed, frozen unit manifest. A successor
-- attempt copies only successful units; the original evidence is never changed.
CREATE TABLE knowledge_office_parse_tasks (
    import_job_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    source_id UUID NOT NULL,
    source_version_id UUID NOT NULL,
    object_id UUID NOT NULL,
    object_version BIGINT NOT NULL,
    input_sha256 TEXT NOT NULL CHECK (input_sha256 ~ '^[0-9a-f]{64}$'),
    media_type TEXT NOT NULL CHECK (media_type IN (
        'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
        'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet'
    )),
    parser_profile TEXT NOT NULL CHECK (length(parser_profile) BETWEEN 1 AND 200),
    manifest_schema TEXT,
    manifest JSONB CHECK (manifest IS NULL OR jsonb_typeof(manifest)='object'),
    unit_count INTEGER CHECK (unit_count BETWEEN 1 AND 20000),
    lease_id UUID,
    fencing_token BIGINT NOT NULL DEFAULT 0 CHECK (fencing_token >= 0),
    released_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id,tenant_id,project_id,import_job_id)
        REFERENCES knowledge_import_jobs (operator_id,tenant_id,project_id,import_job_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id,tenant_id,project_id,source_version_id)
        REFERENCES knowledge_source_versions (operator_id,tenant_id,project_id,source_version_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id,tenant_id,project_id,object_id)
        REFERENCES knowledge_stored_objects (operator_id,tenant_id,project_id,object_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id,tenant_id,project_id,released_id)
        REFERENCES knowledge_releases (operator_id,tenant_id,project_id,knowledge_release_id) ON DELETE RESTRICT,
    UNIQUE (operator_id,tenant_id,project_id,import_job_id),
    UNIQUE (operator_id,tenant_id,project_id,source_version_id),
    CHECK ((manifest_schema IS NULL) = (unit_count IS NULL)
       AND (manifest IS NULL) = (unit_count IS NULL))
);
CREATE INDEX knowledge_office_parse_tasks_created_idx
    ON knowledge_office_parse_tasks (created_at,import_job_id);
CREATE INDEX knowledge_import_jobs_office_retry_idx
    ON knowledge_import_jobs (operator_id,tenant_id,project_id,resumed_from)
    WHERE resumed_from IS NOT NULL;

CREATE FUNCTION knowledge_office_manifest_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.operator_id IS DISTINCT FROM OLD.operator_id
        OR NEW.tenant_id IS DISTINCT FROM OLD.tenant_id
        OR NEW.project_id IS DISTINCT FROM OLD.project_id
        OR NEW.source_id IS DISTINCT FROM OLD.source_id
        OR NEW.source_version_id IS DISTINCT FROM OLD.source_version_id
        OR NEW.object_id IS DISTINCT FROM OLD.object_id
        OR NEW.object_version IS DISTINCT FROM OLD.object_version
        OR NEW.input_sha256 IS DISTINCT FROM OLD.input_sha256
        OR NEW.media_type IS DISTINCT FROM OLD.media_type
        OR NEW.parser_profile IS DISTINCT FROM OLD.parser_profile
        OR (OLD.released_id IS NOT NULL AND NEW.released_id IS DISTINCT FROM OLD.released_id)
        OR (OLD.manifest IS NOT NULL AND (
            NEW.manifest IS DISTINCT FROM OLD.manifest
            OR NEW.unit_count IS DISTINCT FROM OLD.unit_count
            OR NEW.manifest_schema IS DISTINCT FROM OLD.manifest_schema
        ))
        OR ((NEW.manifest IS DISTINCT FROM OLD.manifest) AND EXISTS (
            SELECT 1 FROM knowledge_import_jobs job
            WHERE job.import_job_id=OLD.import_job_id AND job.status<>'running'
        ))
    THEN
        RAISE EXCEPTION 'Office parse input and frozen manifest are immutable';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER knowledge_office_manifest_no_rewrite BEFORE UPDATE ON knowledge_office_parse_tasks
    FOR EACH ROW EXECUTE FUNCTION knowledge_office_manifest_immutable();

CREATE TABLE knowledge_office_parse_units (
    import_job_id UUID NOT NULL,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal BETWEEN 0 AND 19999),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','succeeded','failed')),
    result JSONB CHECK (result IS NULL OR jsonb_typeof(result) = 'object'),
    result_sha256 TEXT CHECK (result_sha256 ~ '^[0-9a-f]{64}$'),
    error_code TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (import_job_id,ordinal),
    FOREIGN KEY (operator_id,tenant_id,project_id,import_job_id)
        REFERENCES knowledge_office_parse_tasks (operator_id,tenant_id,project_id,import_job_id) ON DELETE RESTRICT,
    CHECK ((status='pending' AND result IS NULL AND result_sha256 IS NULL AND error_code IS NULL)
        OR (status='succeeded' AND result IS NOT NULL AND result_sha256 IS NOT NULL AND error_code IS NULL)
        OR (status='failed' AND result IS NOT NULL AND result_sha256 IS NOT NULL AND error_code IS NOT NULL))
);
CREATE INDEX knowledge_office_parse_units_scope_idx
    ON knowledge_office_parse_units (operator_id,tenant_id,project_id,import_job_id,ordinal);

CREATE FUNCTION knowledge_office_unit_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='DELETE' OR OLD.status='succeeded'
        OR EXISTS (
            SELECT 1 FROM knowledge_office_parse_tasks task
            JOIN knowledge_import_jobs job ON job.import_job_id=task.import_job_id
            WHERE task.import_job_id=OLD.import_job_id
              AND (task.released_id IS NOT NULL OR job.status<>'running')
        )
    THEN
        RAISE EXCEPTION 'sealed Office unit outcomes are immutable; create a successor import job';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER knowledge_office_unit_no_rewrite BEFORE UPDATE OR DELETE ON knowledge_office_parse_units
    FOR EACH ROW EXECUTE FUNCTION knowledge_office_unit_immutable();

-- A parse attempt freezes the committed object, its verified bytes hash and
-- parser identity. Retries create a successor job/version, never rewrite an
-- already released source version or page.
CREATE TABLE knowledge_pdf_parse_tasks (
    import_job_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    source_id UUID NOT NULL,
    source_version_id UUID NOT NULL,
    object_id UUID NOT NULL,
    object_version BIGINT NOT NULL,
    input_sha256 TEXT NOT NULL CHECK (input_sha256 ~ '^[0-9a-f]{64}$'),
    parser_profile TEXT NOT NULL CHECK (length(parser_profile) BETWEEN 1 AND 200),
    manifest_schema TEXT,
    page_count INTEGER CHECK (page_count BETWEEN 1 AND 10000),
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
    CHECK ((manifest_schema IS NULL) = (page_count IS NULL))
);
CREATE INDEX knowledge_pdf_parse_tasks_created_idx
    ON knowledge_pdf_parse_tasks (created_at,import_job_id);

CREATE TABLE knowledge_pdf_parse_pages (
    import_job_id UUID NOT NULL,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    page INTEGER NOT NULL CHECK (page BETWEEN 1 AND 10000),
    status TEXT NOT NULL CHECK (status IN ('succeeded','failed')),
    text TEXT,
    error_code TEXT,
    text_sha256 TEXT CHECK (text_sha256 ~ '^[0-9a-f]{64}$'),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (import_job_id,page),
    FOREIGN KEY (operator_id,tenant_id,project_id,import_job_id)
        REFERENCES knowledge_pdf_parse_tasks (operator_id,tenant_id,project_id,import_job_id) ON DELETE RESTRICT,
    CHECK ((status='succeeded' AND text IS NOT NULL AND error_code IS NULL AND text_sha256 IS NOT NULL)
        OR (status='failed' AND text IS NULL AND error_code IS NOT NULL AND text_sha256 IS NULL))
);
CREATE INDEX knowledge_pdf_parse_pages_scope_idx
    ON knowledge_pdf_parse_pages (operator_id,tenant_id,project_id,import_job_id,page);

CREATE FUNCTION knowledge_pdf_page_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='DELETE' OR OLD.status='succeeded'
        OR EXISTS (
            SELECT 1 FROM knowledge_pdf_parse_tasks task
            JOIN knowledge_import_jobs job ON job.import_job_id=task.import_job_id
            WHERE task.import_job_id=OLD.import_job_id
              AND (task.released_id IS NOT NULL OR job.status<>'running')
        )
    THEN
        RAISE EXCEPTION 'sealed PDF page outcomes are immutable; create a successor import job';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER knowledge_pdf_page_no_rewrite BEFORE UPDATE OR DELETE ON knowledge_pdf_parse_pages
    FOR EACH ROW EXECUTE FUNCTION knowledge_pdf_page_immutable();

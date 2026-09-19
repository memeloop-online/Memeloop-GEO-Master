-- W03 verified knowledge-import foundation.  These tables carry explicit
-- operator/tenant/project columns and repositories set transaction-local
-- scope.  RLS itself remains deliberately deferred (see 0004).

ALTER TABLE outbox_events
    ADD COLUMN IF NOT EXISTS cycle_id UUID;

CREATE TABLE knowledge_upload_sessions (
    upload_session_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    revision BIGINT NOT NULL CHECK (revision > 0),
    filename TEXT NOT NULL CHECK (length(trim(filename)) BETWEEN 1 AND 255),
    declared_media_type TEXT NOT NULL CHECK (length(trim(declared_media_type)) BETWEEN 1 AND 255),
    expected_size BIGINT NOT NULL CHECK (expected_size > 0 AND expected_size <= 104857600),
    expected_sha256 TEXT NOT NULL CHECK (expected_sha256 ~ '^[0-9a-f]{64}$'),
    purpose TEXT NOT NULL CHECK (purpose IN ('public', 'internal')),
    state TEXT NOT NULL CHECK (state IN ('created', 'uploading', 'uploaded', 'committed', 'failed', 'expired', 'cancelled')),
    expires_at TIMESTAMPTZ NOT NULL,
    staging_object_ref TEXT,
    committed_object_id UUID,
    operation_id UUID,
    completion_idempotency_key_hash TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id, upload_session_id)
);
CREATE INDEX knowledge_upload_sessions_scope_expiry_idx
    ON knowledge_upload_sessions (operator_id, tenant_id, project_id, expires_at);

-- The first durable adapter is a database blob, keyed only by a server
-- generated session ID.  Public APIs never accept backend paths or keys.
CREATE TABLE knowledge_upload_blobs (
    upload_session_id UUID PRIMARY KEY
        REFERENCES knowledge_upload_sessions(upload_session_id) ON DELETE CASCADE,
    content BYTEA NOT NULL,
    actual_size BIGINT NOT NULL CHECK (actual_size >= 0),
    sha256 TEXT NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE knowledge_stored_objects (
    object_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    object_version BIGINT NOT NULL CHECK (object_version > 0),
    backend TEXT NOT NULL,
    opaque_key TEXT NOT NULL,
    actual_size BIGINT NOT NULL CHECK (actual_size >= 0),
    detected_media_type TEXT NOT NULL,
    sha256 TEXT NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    state TEXT NOT NULL CHECK (state IN ('staged', 'committed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id, object_id),
    UNIQUE (operator_id, tenant_id, project_id, opaque_key)
);
CREATE INDEX knowledge_stored_objects_scope_created_idx
    ON knowledge_stored_objects (operator_id, tenant_id, project_id, created_at DESC);

CREATE TABLE knowledge_sources (
    source_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    revision BIGINT NOT NULL CHECK (revision > 0),
    kind TEXT NOT NULL CHECK (kind IN ('file', 'url', 'text', 'object', 'knowledge_collection', 'manual')),
    name TEXT NOT NULL,
    purpose TEXT NOT NULL CHECK (purpose IN ('public', 'internal')),
    state TEXT NOT NULL CHECK (state IN ('active', 'removed')),
    locator JSONB NOT NULL,
    current_version_id UUID,
    sync_enabled BOOLEAN NOT NULL DEFAULT false,
    next_sync_at TIMESTAMPTZ,
    last_sync_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id, source_id)
);
CREATE INDEX knowledge_sources_scope_created_idx
    ON knowledge_sources (operator_id, tenant_id, project_id, created_at DESC);

CREATE TABLE knowledge_source_versions (
    source_version_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    source_id UUID NOT NULL,
    version BIGINT NOT NULL CHECK (version > 0),
    object_id UUID,
    object_version BIGINT,
    content_sha256 TEXT NOT NULL CHECK (content_sha256 ~ '^[0-9a-f]{64}$'),
    captured_at TIMESTAMPTZ NOT NULL,
    original_url TEXT,
    parent_version_id UUID,
    parser_version TEXT NOT NULL,
    extraction_version TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id, project_id, source_id)
        REFERENCES knowledge_sources (operator_id, tenant_id, project_id, source_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, object_id)
        REFERENCES knowledge_stored_objects (operator_id, tenant_id, project_id, object_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, parent_version_id)
        REFERENCES knowledge_source_versions (operator_id, tenant_id, project_id, source_version_id) ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id, source_version_id),
    UNIQUE (operator_id, tenant_id, project_id, source_id, version)
);
ALTER TABLE knowledge_sources
    ADD CONSTRAINT knowledge_sources_current_version_scope_fk
    FOREIGN KEY (operator_id, tenant_id, project_id, current_version_id)
    REFERENCES knowledge_source_versions (operator_id, tenant_id, project_id, source_version_id)
    DEFERRABLE INITIALLY DEFERRED;
CREATE INDEX knowledge_source_versions_scope_source_idx
    ON knowledge_source_versions (operator_id, tenant_id, project_id, source_id, version DESC);

CREATE TABLE knowledge_import_jobs (
    import_job_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    operation_id UUID NOT NULL,
    source_id UUID NOT NULL,
    source_version_id UUID,
    stage TEXT NOT NULL CHECK (stage IN ('acquire', 'parse', 'extract', 'index', 'release')),
    status TEXT NOT NULL CHECK (status IN ('queued', 'running', 'partial', 'succeeded', 'failed', 'cancelled')),
    attempt INTEGER NOT NULL CHECK (attempt >= 0),
    lease_until TIMESTAMPTZ,
    input_hash TEXT NOT NULL,
    stage_output_refs JSONB NOT NULL DEFAULT '[]'::JSONB,
    completed_units INTEGER NOT NULL DEFAULT 0 CHECK (completed_units >= 0),
    failed_units INTEGER NOT NULL DEFAULT 0 CHECK (failed_units >= 0),
    errors JSONB NOT NULL DEFAULT '[]'::JSONB,
    resumed_from UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operation_id) REFERENCES operations(operation_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, source_id)
        REFERENCES knowledge_sources (operator_id, tenant_id, project_id, source_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, source_version_id)
        REFERENCES knowledge_source_versions (operator_id, tenant_id, project_id, source_version_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, resumed_from)
        REFERENCES knowledge_import_jobs (operator_id, tenant_id, project_id, import_job_id) ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id, import_job_id)
);
CREATE INDEX knowledge_import_jobs_scope_status_idx
    ON knowledge_import_jobs (operator_id, tenant_id, project_id, status, updated_at DESC);

-- Stable client item IDs and upload-complete keys have a durable receipt.
-- The key itself is never retained; request_hash is a SHA-256 digest.
CREATE TABLE knowledge_import_receipts (
    knowledge_import_receipt_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    action TEXT NOT NULL CHECK (action IN ('import_item', 'upload_complete')),
    target_id UUID NOT NULL,
    client_item_id TEXT,
    request_hash TEXT NOT NULL CHECK (request_hash ~ '^[0-9a-f]{64}$'),
    acceptance JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id, action, target_id)
);
CREATE UNIQUE INDEX knowledge_import_receipts_import_item_client_idx
    ON knowledge_import_receipts (operator_id, tenant_id, project_id, client_item_id)
    WHERE action = 'import_item';
CREATE INDEX knowledge_import_receipts_scope_created_idx
    ON knowledge_import_receipts (operator_id, tenant_id, project_id, created_at DESC);

CREATE TABLE knowledge_chunks (
    chunk_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    source_version_id UUID NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    kind TEXT NOT NULL CHECK (kind IN ('paragraph', 'table', 'image_description')),
    text TEXT NOT NULL,
    text_hash TEXT NOT NULL CHECK (text_hash ~ '^[0-9a-f]{64}$'),
    locator JSONB NOT NULL,
    product_ids JSONB NOT NULL DEFAULT '[]'::JSONB,
    market TEXT,
    language TEXT,
    extraction_method TEXT NOT NULL,
    confidence REAL NOT NULL CHECK (confidence >= 0 AND confidence <= 1),
    FOREIGN KEY (operator_id, tenant_id, project_id, source_version_id)
        REFERENCES knowledge_source_versions (operator_id, tenant_id, project_id, source_version_id) ON DELETE CASCADE,
    UNIQUE (operator_id, tenant_id, project_id, chunk_id),
    UNIQUE (operator_id, tenant_id, project_id, source_version_id, ordinal)
);
CREATE INDEX knowledge_chunks_scope_version_idx
    ON knowledge_chunks (operator_id, tenant_id, project_id, source_version_id, ordinal);

CREATE TABLE knowledge_products (
    product_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    revision BIGINT NOT NULL CHECK (revision > 0),
    name TEXT NOT NULL,
    model TEXT,
    aliases JSONB NOT NULL DEFAULT '[]'::JSONB,
    state TEXT NOT NULL CHECK (state IN ('active', 'archived')),
    evidence_refs JSONB NOT NULL DEFAULT '[]'::JSONB,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id, product_id)
);

CREATE TABLE knowledge_facts (
    fact_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    revision BIGINT NOT NULL CHECK (revision > 0),
    subject_id TEXT NOT NULL,
    product_id UUID,
    model TEXT,
    attribute TEXT NOT NULL,
    typed_value JSONB NOT NULL,
    unit TEXT,
    market TEXT,
    language TEXT,
    currency TEXT,
    effective_from TIMESTAMPTZ,
    effective_to TIMESTAMPTZ,
    status TEXT NOT NULL CHECK (status IN ('candidate', 'confirmed', 'conflicted', 'superseded')),
    pinned BOOLEAN NOT NULL DEFAULT false,
    evidence_refs JSONB NOT NULL DEFAULT '[]'::JSONB,
    supersedes_fact_id UUID,
    FOREIGN KEY (operator_id, tenant_id, project_id, product_id)
        REFERENCES knowledge_products (operator_id, tenant_id, project_id, product_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, supersedes_fact_id)
        REFERENCES knowledge_facts (operator_id, tenant_id, project_id, fact_id) ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id, fact_id)
);

CREATE TABLE knowledge_releases (
    knowledge_release_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    sequence BIGINT NOT NULL CHECK (sequence > 0),
    previous_release_id UUID,
    index_build_id TEXT NOT NULL,
    pipeline_versions JSONB NOT NULL,
    content_hash TEXT NOT NULL CHECK (content_hash ~ '^[0-9a-f]{64}$'),
    coverage JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, previous_release_id)
        REFERENCES knowledge_releases (operator_id, tenant_id, project_id, knowledge_release_id) ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id, knowledge_release_id),
    UNIQUE (operator_id, tenant_id, project_id, sequence)
);
CREATE TABLE knowledge_release_source_versions (
    knowledge_release_id UUID NOT NULL,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    source_version_id UUID NOT NULL,
    PRIMARY KEY (knowledge_release_id, source_version_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, knowledge_release_id)
        REFERENCES knowledge_releases (operator_id, tenant_id, project_id, knowledge_release_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id, source_version_id)
        REFERENCES knowledge_source_versions (operator_id, tenant_id, project_id, source_version_id) ON DELETE RESTRICT
);
CREATE TABLE knowledge_release_facts (
    knowledge_release_id UUID NOT NULL,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    fact_id UUID NOT NULL,
    fact_revision BIGINT NOT NULL,
    PRIMARY KEY (knowledge_release_id, fact_id, fact_revision),
    FOREIGN KEY (operator_id, tenant_id, project_id, knowledge_release_id)
        REFERENCES knowledge_releases (operator_id, tenant_id, project_id, knowledge_release_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id, fact_id)
        REFERENCES knowledge_facts (operator_id, tenant_id, project_id, fact_id) ON DELETE RESTRICT
);
CREATE TABLE knowledge_current_releases (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    knowledge_release_id UUID NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (operator_id, tenant_id, project_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, knowledge_release_id)
        REFERENCES knowledge_releases (operator_id, tenant_id, project_id, knowledge_release_id) ON DELETE RESTRICT
);

-- A release seals a finite W03 planning skeleton.  It intentionally has no
-- brief/content revisions: those are W05/W06 concerns.
CREATE TABLE document_manifest_items (
    document_manifest_item_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    manifest_id UUID NOT NULL,
    knowledge_release_id UUID NOT NULL,
    document_key TEXT NOT NULL,
    content_type TEXT NOT NULL,
    product_id UUID,
    market TEXT NOT NULL,
    language TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('planned', 'blocked', 'deferred', 'not_applicable')),
    block_reason TEXT,
    dependency_hash TEXT NOT NULL,
    brief_revision_id UUID,
    content_revision_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id, project_id, manifest_id)
        REFERENCES document_manifests (operator_id, tenant_id, project_id, manifest_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id, knowledge_release_id)
        REFERENCES knowledge_releases (operator_id, tenant_id, project_id, knowledge_release_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, product_id)
        REFERENCES knowledge_products (operator_id, tenant_id, project_id, product_id) ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id, manifest_id, document_key)
);
CREATE INDEX document_manifest_items_manifest_idx
    ON document_manifest_items (operator_id, tenant_id, project_id, manifest_id, state);

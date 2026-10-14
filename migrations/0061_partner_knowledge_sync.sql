-- Inbound service identities are project-bound; no export/publication capability.
CREATE TABLE partner_connections (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    connection_id UUID PRIMARY KEY,
    knowledge_write BOOLEAN NOT NULL DEFAULT true,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (operator_id,tenant_id,project_id,connection_id),
    FOREIGN KEY (operator_id,tenant_id,project_id)
        REFERENCES projects(operator_id,tenant_id,project_id)
);
CREATE TABLE partner_keys (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    key_id UUID PRIMARY KEY,
    verification_hash TEXT NOT NULL,
    expires_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (operator_id,tenant_id,project_id,connection_id,key_id),
    FOREIGN KEY (operator_id,tenant_id,project_id,connection_id)
        REFERENCES partner_connections(operator_id,tenant_id,project_id,connection_id)
);
CREATE TABLE partner_sync_operations (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    operation_id UUID PRIMARY KEY,
    accepted_key_id UUID NOT NULL,
    idempotency_hash TEXT NOT NULL CHECK (idempotency_hash ~ '^[0-9a-f]{64}$'),
    request_hash TEXT NOT NULL CHECK (request_hash ~ '^[0-9a-f]{64}$'),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','running','completed')),
    item_count INTEGER NOT NULL CHECK (item_count BETWEEN 1 AND 100),
    completed_count INTEGER NOT NULL DEFAULT 0 CHECK (completed_count BETWEEN 0 AND item_count),
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    failure_count INTEGER NOT NULL DEFAULT 0 CHECK (failure_count >= 0),
    reason_code TEXT CHECK (reason_code IS NULL OR reason_code='storage_unavailable'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (operator_id,tenant_id,project_id,connection_id,operation_id),
    UNIQUE (operator_id,tenant_id,project_id,connection_id,idempotency_hash),
    FOREIGN KEY (operator_id,tenant_id,project_id,connection_id,accepted_key_id)
        REFERENCES partner_keys(operator_id,tenant_id,project_id,connection_id,key_id)
);
CREATE INDEX partner_sync_due_idx ON partner_sync_operations(next_attempt_at,operation_id)
    WHERE status <> 'completed';
CREATE TABLE partner_sync_items (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    operation_id UUID NOT NULL,
    item_index INTEGER NOT NULL CHECK (item_index BETWEEN 0 AND 99),
    input JSONB NOT NULL,
    intake_reason_code TEXT CHECK (intake_reason_code IS NULL OR intake_reason_code IN
        ('invalid_source_identity','invalid_source_name','unsupported_media_type','invalid_source_text',
         'content_digest_mismatch','source_identity_conflict','stale_source_sequence')),
    result JSONB,
    completed_at TIMESTAMPTZ,
    PRIMARY KEY (operator_id,tenant_id,project_id,connection_id,operation_id,item_index),
    FOREIGN KEY (operator_id,tenant_id,project_id,connection_id,operation_id)
        REFERENCES partner_sync_operations(operator_id,tenant_id,project_id,connection_id,operation_id),
    CHECK ((result IS NULL) = (completed_at IS NULL))
);
CREATE TABLE partner_source_mappings (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    external_source_id TEXT NOT NULL CHECK (octet_length(external_source_id) BETWEEN 1 AND 256),
    source_id UUID,
    accepted_sequence BIGINT NOT NULL CHECK (accepted_sequence >= 0),
    reserved_sequence BIGINT NOT NULL CHECK (reserved_sequence > 0 AND reserved_sequence >= accepted_sequence),
    internal_fence_sequence BIGINT CHECK (internal_fence_sequence > 0),
    revoke_fence_sequence BIGINT CHECK (revoke_fence_sequence > 0),
    revoked BOOLEAN NOT NULL,
    PRIMARY KEY (operator_id,tenant_id,project_id,connection_id,external_source_id),
    FOREIGN KEY (operator_id,tenant_id,project_id,connection_id)
        REFERENCES partner_connections(operator_id,tenant_id,project_id,connection_id),
    FOREIGN KEY (operator_id,tenant_id,project_id,source_id)
        REFERENCES knowledge_sources(operator_id,tenant_id,project_id,source_id)
);
-- Intake owns version/sequence admission before any worker can materialize it.
-- Terminal results retain business rejections as well as successful resources.
CREATE TABLE partner_source_intake_claims (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    external_source_id TEXT NOT NULL,
    external_version TEXT NOT NULL CHECK (octet_length(external_version) BETWEEN 1 AND 256),
    source_sequence BIGINT NOT NULL CHECK (source_sequence > 0),
    semantic_hash TEXT NOT NULL CHECK (semantic_hash ~ '^[0-9a-f]{64}$'),
    operation_id UUID NOT NULL,
    item_index INTEGER NOT NULL,
    result JSONB,
    PRIMARY KEY (operator_id,tenant_id,project_id,connection_id,external_source_id,external_version),
    UNIQUE (operator_id,tenant_id,project_id,connection_id,external_source_id,source_sequence),
    UNIQUE (operator_id,tenant_id,project_id,connection_id,operation_id,item_index),
    FOREIGN KEY (operator_id,tenant_id,project_id,connection_id,external_source_id)
        REFERENCES partner_source_mappings(operator_id,tenant_id,project_id,connection_id,external_source_id),
    FOREIGN KEY (operator_id,tenant_id,project_id,connection_id,operation_id,item_index)
        REFERENCES partner_sync_items(operator_id,tenant_id,project_id,connection_id,operation_id,item_index)
);
CREATE INDEX partner_source_intake_pending_idx ON partner_source_intake_claims
    (operator_id,tenant_id,project_id,connection_id,external_source_id,source_sequence)
    WHERE result IS NULL;
CREATE TABLE partner_source_events (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    external_source_id TEXT NOT NULL,
    external_version TEXT NOT NULL CHECK (octet_length(external_version) BETWEEN 1 AND 256),
    source_sequence BIGINT NOT NULL CHECK (source_sequence > 0),
    semantic_hash TEXT NOT NULL CHECK (semantic_hash ~ '^[0-9a-f]{64}$'),
    operation_id UUID NOT NULL,
    item_index INTEGER NOT NULL,
    source_id UUID,
    source_version_id UUID,
    import_job_id UUID,
    knowledge_release_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (operator_id,tenant_id,project_id,connection_id,external_source_id,external_version),
    UNIQUE (operator_id,tenant_id,project_id,connection_id,external_source_id,source_sequence),
    FOREIGN KEY (operator_id,tenant_id,project_id,connection_id,external_source_id)
        REFERENCES partner_source_mappings(operator_id,tenant_id,project_id,connection_id,external_source_id),
    FOREIGN KEY (operator_id,tenant_id,project_id,connection_id,operation_id,item_index)
        REFERENCES partner_sync_items(operator_id,tenant_id,project_id,connection_id,operation_id,item_index),
    FOREIGN KEY (operator_id,tenant_id,project_id,source_id)
        REFERENCES knowledge_sources(operator_id,tenant_id,project_id,source_id),
    FOREIGN KEY (operator_id,tenant_id,project_id,source_id,source_version_id)
        REFERENCES knowledge_source_versions(operator_id,tenant_id,project_id,source_id,source_version_id),
    FOREIGN KEY (operator_id,tenant_id,project_id,import_job_id)
        REFERENCES knowledge_import_jobs(operator_id,tenant_id,project_id,import_job_id),
    FOREIGN KEY (operator_id,tenant_id,project_id,knowledge_release_id)
        REFERENCES knowledge_releases(operator_id,tenant_id,project_id,knowledge_release_id),
    CHECK (source_version_id IS NULL OR source_id IS NOT NULL)
);
CREATE TABLE partner_audit_events (
    audit_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    connection_id UUID NOT NULL,
    key_id UUID,
    event TEXT NOT NULL CHECK (event IN ('connection_created','connection_revoked','key_issued','key_revoked')),
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id,tenant_id,project_id,connection_id)
        REFERENCES partner_connections(operator_id,tenant_id,project_id,connection_id),
    FOREIGN KEY (operator_id,tenant_id,project_id,connection_id,key_id)
        REFERENCES partner_keys(operator_id,tenant_id,project_id,connection_id,key_id)
);
-- API scoped operations set transaction-local scope. Authentication and worker
-- discovery use the trusted backend role; clients never connect to PostgreSQL.
DO $$
DECLARE table_name TEXT;
BEGIN
    FOREACH table_name IN ARRAY ARRAY[
        'partner_connections','partner_keys','partner_sync_operations',
        'partner_sync_items','partner_source_mappings','partner_source_intake_claims',
        'partner_source_events','partner_audit_events'
    ] LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY',table_name);
        EXECUTE format(
            'CREATE POLICY partner_project_scope ON %I USING (
                operator_id = NULLIF(current_setting(''geo.operator_id'',true),'''')::uuid
                AND tenant_id = NULLIF(current_setting(''geo.tenant_id'',true),'''')::uuid
                AND project_id = NULLIF(current_setting(''geo.project_id'',true),'''')::uuid
            ) WITH CHECK (
                operator_id = NULLIF(current_setting(''geo.operator_id'',true),'''')::uuid
                AND tenant_id = NULLIF(current_setting(''geo.tenant_id'',true),'''')::uuid
                AND project_id = NULLIF(current_setting(''geo.project_id'',true),'''')::uuid
            )',table_name);
    END LOOP;
END $$;

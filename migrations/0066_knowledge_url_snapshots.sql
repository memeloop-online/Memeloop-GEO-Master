-- URL snapshots are server acquisitions, independent of upload sessions.
CREATE TABLE knowledge_url_snapshot_bytes (
    object_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    object_version BIGINT NOT NULL CHECK (object_version > 0),
    content BYTEA NOT NULL,
    actual_size BIGINT NOT NULL CHECK (actual_size > 0 AND actual_size <= 104857600),
    sha256 TEXT NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (octet_length(content) = actual_size),
    FOREIGN KEY (operator_id, tenant_id, project_id, object_id)
        REFERENCES knowledge_stored_objects (operator_id, tenant_id, project_id, object_id)
        ON DELETE RESTRICT
);

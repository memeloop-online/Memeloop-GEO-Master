CREATE TABLE project_serp_settings (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    source_key TEXT NOT NULL CHECK (octet_length(source_key) BETWEEN 1 AND 128),
    revision BIGINT NOT NULL CHECK (revision > 0),
    provider TEXT NOT NULL CHECK (provider = 'dataforseo'),
    enabled BOOLEAN NOT NULL,
    protocol_defaults JSONB NOT NULL,
    active_credential_revision BIGINT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (operator_id, tenant_id, project_id, source_key),
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id),
    CHECK (NOT enabled OR active_credential_revision IS NOT NULL)
);

-- Never overwrite old credentials: submitted task IDs remain bound to the
-- credential version used for their one-time submission.
CREATE TABLE project_serp_credentials (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    source_key TEXT NOT NULL,
    credential_revision BIGINT NOT NULL CHECK (credential_revision > 0),
    encrypted_credentials BYTEA NOT NULL CHECK (octet_length(encrypted_credentials) BETWEEN 1 AND 16384),
    stored_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (operator_id, tenant_id, project_id, source_key, credential_revision),
    FOREIGN KEY (operator_id, tenant_id, project_id, source_key)
        REFERENCES project_serp_settings (operator_id, tenant_id, project_id, source_key)
);
ALTER TABLE project_serp_settings ADD CONSTRAINT project_serp_active_credentials
    FOREIGN KEY (operator_id, tenant_id, project_id, source_key, active_credential_revision)
    REFERENCES project_serp_credentials (operator_id, tenant_id, project_id, source_key, credential_revision)
    DEFERRABLE INITIALLY DEFERRED;

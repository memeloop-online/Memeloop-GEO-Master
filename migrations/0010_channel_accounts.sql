-- Account metadata and browser sessions are scoped to one project.
-- Browser storage state and proxy credentials are AEAD envelopes (BYTEA),
-- never JSON metadata or public response fields.
CREATE TABLE channel_groups (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    group_id UUID NOT NULL,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 120),
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (operator_id, tenant_id, project_id, group_id),
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE CASCADE
);

CREATE TABLE channel_settings (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    default_group_id UUID,
    metadata JSONB NOT NULL,
    encrypted_proxy BYTEA,
    PRIMARY KEY (operator_id, tenant_id, project_id),
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id, default_group_id)
        REFERENCES channel_groups (operator_id, tenant_id, project_id, group_id) ON DELETE SET NULL (default_group_id)
);

CREATE TABLE channel_accounts (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    account_id UUID NOT NULL,
    platform TEXT NOT NULL,
    platform_account_id TEXT,
    group_id UUID,
    metadata JSONB NOT NULL,
    encrypted_session BYTEA,
    encrypted_proxy BYTEA,
    PRIMARY KEY (operator_id, tenant_id, project_id, account_id),
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id, group_id)
        REFERENCES channel_groups (operator_id, tenant_id, project_id, group_id) ON DELETE SET NULL (group_id)
);
CREATE UNIQUE INDEX channel_accounts_identity_unique
    ON channel_accounts (operator_id, tenant_id, platform, platform_account_id)
    WHERE platform_account_id IS NOT NULL;
CREATE INDEX channel_accounts_project_idx
    ON channel_accounts (operator_id, tenant_id, project_id);

CREATE TABLE channel_login_sessions (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    session_id UUID NOT NULL,
    account_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (operator_id, tenant_id, project_id, session_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, account_id)
        REFERENCES channel_accounts (operator_id, tenant_id, project_id, account_id) ON DELETE CASCADE
);

-- Operator-owned shared pool is not a customer project. Only explicit
-- assignments below expose public metadata to a customer project.
CREATE TABLE operator_channel_groups (
    operator_id UUID NOT NULL REFERENCES operators(operator_id) ON DELETE CASCADE,
    group_id UUID NOT NULL,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 120),
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (operator_id, group_id)
);

CREATE TABLE operator_channel_accounts (
    operator_id UUID NOT NULL REFERENCES operators(operator_id) ON DELETE CASCADE,
    account_id UUID NOT NULL,
    platform TEXT NOT NULL,
    platform_account_id TEXT,
    group_id UUID,
    metadata JSONB NOT NULL,
    encrypted_session BYTEA,
    encrypted_proxy BYTEA,
    PRIMARY KEY (operator_id, account_id),
    FOREIGN KEY (operator_id, group_id)
        REFERENCES operator_channel_groups(operator_id, group_id) ON DELETE SET NULL (group_id)
);
CREATE UNIQUE INDEX operator_channel_identity_unique
    ON operator_channel_accounts (operator_id, platform, platform_account_id)
    WHERE platform_account_id IS NOT NULL;

CREATE TABLE operator_channel_assignments (
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    account_id UUID NOT NULL,
    PRIMARY KEY (operator_id, tenant_id, project_id, account_id),
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects(operator_id, tenant_id, project_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, account_id)
        REFERENCES operator_channel_accounts(operator_id, account_id) ON DELETE CASCADE
);

CREATE TABLE operator_channel_login_sessions (
    operator_id UUID NOT NULL,
    session_id UUID NOT NULL,
    account_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (operator_id, session_id),
    FOREIGN KEY (operator_id, account_id)
        REFERENCES operator_channel_accounts(operator_id, account_id) ON DELETE CASCADE
);

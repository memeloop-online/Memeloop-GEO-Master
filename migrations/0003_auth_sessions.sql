-- W01 authenticated identities, operator host routing, tenant memberships and
-- opaque browser sessions.  Passwords are always supplied as a password hash
-- by the application; this migration intentionally contains no default
-- password or demo credential.

CREATE TABLE users (
    user_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL REFERENCES operators(operator_id) ON DELETE CASCADE,
    email TEXT NOT NULL,
    display_name TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (email = lower(email)),
    CHECK (length(trim(email)) > 3),
    UNIQUE (operator_id, email)
);

CREATE TABLE operator_hosts (
    host TEXT PRIMARY KEY,
    operator_id UUID NOT NULL REFERENCES operators(operator_id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (host = lower(host)),
    CHECK (length(trim(host)) > 0)
);

CREATE INDEX operator_hosts_operator_id_idx ON operator_hosts (operator_id);

CREATE TABLE memberships (
    membership_id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(user_id) ON DELETE CASCADE,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    role TEXT NOT NULL CHECK (role IN (
        'customer_admin', 'customer_member', 'customer_read_only',
        'operator', 'resource_admin', 'oem_admin'
    )),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id)
        ON DELETE CASCADE,
    UNIQUE (user_id, operator_id, tenant_id)
);

CREATE INDEX memberships_user_operator_idx
    ON memberships (user_id, operator_id, active);
CREATE INDEX memberships_tenant_idx
    ON memberships (operator_id, tenant_id, active);

CREATE TABLE sessions (
    session_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL REFERENCES operators(operator_id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(user_id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    csrf_token TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX sessions_user_id_idx ON sessions (user_id, expires_at DESC);
CREATE INDEX sessions_operator_id_idx ON sessions (operator_id, expires_at DESC);
CREATE INDEX sessions_active_idx ON sessions (expires_at)
    WHERE revoked_at IS NULL;

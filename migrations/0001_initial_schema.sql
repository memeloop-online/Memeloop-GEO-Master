-- W01 persistence foundation. Business rows carry the operator and tenant
-- scope so every query can enforce the same boundary as the domain contract.

CREATE TABLE operators (
    operator_id UUID PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE tenants (
    tenant_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL REFERENCES operators(operator_id) ON DELETE CASCADE,
    slug TEXT NOT NULL,
    display_name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (operator_id, tenant_id),
    UNIQUE (operator_id, slug)
);

CREATE INDEX tenants_operator_id_idx ON tenants (operator_id);

CREATE TABLE projects (
    project_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    slug TEXT NOT NULL,
    display_name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id)
        ON DELETE CASCADE,
    UNIQUE (operator_id, tenant_id, project_id),
    UNIQUE (operator_id, tenant_id, slug)
);

CREATE INDEX projects_tenant_id_idx ON projects (operator_id, tenant_id, created_at DESC);

CREATE TABLE operations (
    operation_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID,
    kind TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('queued', 'running', 'succeeded', 'failed')),
    result JSONB,
    error JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id)
        ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id)
        ON DELETE RESTRICT
);

CREATE INDEX operations_scope_created_at_idx
    ON operations (operator_id, tenant_id, created_at DESC);
CREATE INDEX operations_scope_status_idx
    ON operations (operator_id, tenant_id, status, updated_at DESC);

CREATE TABLE idempotency_records (
    idempotency_record_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID,
    idempotency_key TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('in_flight', 'completed')),
    response_status SMALLINT,
    response_content_type TEXT,
    response_body BYTEA,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id)
        ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id)
        ON DELETE RESTRICT
);

-- A key is unique within an operator/tenant/project scope. COALESCE makes a
-- tenant-wide record (project_id IS NULL) unique under PostgreSQL's NULL rules.
CREATE UNIQUE INDEX idempotency_records_scope_key_idx
    ON idempotency_records (
        operator_id,
        tenant_id,
        COALESCE(project_id, '00000000-0000-0000-0000-000000000000'::UUID),
        idempotency_key
    );
CREATE INDEX idempotency_records_expiry_idx
    ON idempotency_records (expires_at)
    WHERE expires_at IS NOT NULL;

CREATE TABLE outbox_events (
    event_id UUID PRIMARY KEY,
    event_type TEXT NOT NULL,
    schema_version INTEGER NOT NULL CHECK (schema_version > 0),
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID,
    aggregate_id UUID NOT NULL,
    aggregate_version BIGINT NOT NULL CHECK (aggregate_version >= 0),
    occurred_at TIMESTAMPTZ NOT NULL,
    correlation_id UUID NOT NULL,
    causation_id UUID,
    payload_ref TEXT,
    payload JSONB,
    delivery_state TEXT NOT NULL DEFAULT 'pending'
        CHECK (delivery_state IN ('pending', 'published', 'failed')),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    available_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id)
        ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id)
        ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, aggregate_id, aggregate_version, event_type)
);

CREATE INDEX outbox_events_pending_idx
    ON outbox_events (available_at, occurred_at)
    WHERE delivery_state = 'pending';
CREATE INDEX outbox_events_scope_idx
    ON outbox_events (operator_id, tenant_id, occurred_at DESC);

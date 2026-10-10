-- Trusted deployment provisioning only; no public route-management endpoint.
-- A disabled project grant shadows a tenant-wide grant for the same model.
CREATE TABLE tenant_model_routes (
    route_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID,
    model TEXT NOT NULL CHECK (length(model) BETWEEN 1 AND 256 AND model !~ '://'),
    is_default BOOLEAN NOT NULL DEFAULT false,
    enabled BOOLEAN NOT NULL DEFAULT false,
    tenant_external_id TEXT NOT NULL CHECK (length(tenant_external_id) BETWEEN 1 AND 200),
    principal_external_id TEXT NOT NULL CHECK (length(principal_external_id) BETWEEN 1 AND 200),
    key_id UUID NOT NULL,
    credential_generation BIGINT NOT NULL CHECK (credential_generation > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id)
        REFERENCES tenants (operator_id, tenant_id) ON DELETE CASCADE,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE CASCADE
);

CREATE UNIQUE INDEX tenant_model_routes_scoped_model
    ON tenant_model_routes (operator_id, tenant_id,
        COALESCE(project_id, '00000000-0000-0000-0000-000000000000'::UUID), model);
CREATE UNIQUE INDEX tenant_model_routes_scoped_default
    ON tenant_model_routes (operator_id, tenant_id,
        COALESCE(project_id, '00000000-0000-0000-0000-000000000000'::UUID))
    WHERE is_default;
CREATE INDEX tenant_model_routes_scope_lookup
    ON tenant_model_routes (operator_id, tenant_id, project_id);

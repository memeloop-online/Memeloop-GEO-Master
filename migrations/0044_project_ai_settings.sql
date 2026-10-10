CREATE TABLE project_ai_settings (
    operator_id uuid NOT NULL,
    tenant_id uuid NOT NULL,
    project_id uuid NOT NULL,
    usage text NOT NULL CHECK (usage IN ('workbench_content','observation_analysis')),
    revision bigint NOT NULL CHECK (revision > 0),
    mode text NOT NULL CHECK (mode IN ('inherit','custom')),
    model text,
    base_url text,
    encrypted_api_key bytea,
    prefer_connected_account boolean NOT NULL DEFAULT true,
    PRIMARY KEY (operator_id, tenant_id, project_id, usage),
    FOREIGN KEY (operator_id, tenant_id, project_id) REFERENCES projects (operator_id, tenant_id, project_id),
    CHECK (mode <> 'inherit' OR (model IS NULL AND base_url IS NULL AND encrypted_api_key IS NULL))
);

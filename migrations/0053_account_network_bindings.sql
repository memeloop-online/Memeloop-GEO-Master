CREATE TABLE account_network_heads (
    operator_id UUID NOT NULL REFERENCES operators(operator_id),
    account_id UUID NOT NULL,
    scope JSONB NOT NULL,
    version BIGINT NOT NULL DEFAULT 0 CHECK(version >= 0),
    active_binding_id UUID,
    pending_binding_id UUID,
    PRIMARY KEY(operator_id,account_id)
);
CREATE TABLE account_network_bindings (
    operator_id UUID NOT NULL,
    account_id UUID NOT NULL,
    binding_id UUID NOT NULL UNIQUE,
    source JSONB NOT NULL,
    effective_network BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    pool_id UUID,
    member_id UUID,
    revision_id UUID,
    PRIMARY KEY(operator_id,account_id,binding_id),
    FOREIGN KEY(operator_id,account_id) REFERENCES account_network_heads(operator_id,account_id),
    FOREIGN KEY(operator_id,pool_id,member_id,revision_id)
        REFERENCES operator_proxy_revisions(operator_id,pool_id,member_id,revision_id) MATCH SIMPLE,
    CHECK ((pool_id IS NULL AND member_id IS NULL AND revision_id IS NULL)
        OR (pool_id IS NOT NULL AND member_id IS NOT NULL AND revision_id IS NOT NULL))
);
ALTER TABLE account_network_heads ADD CONSTRAINT account_network_active
    FOREIGN KEY(operator_id,account_id,active_binding_id)
    REFERENCES account_network_bindings(operator_id,account_id,binding_id);
ALTER TABLE account_network_heads ADD CONSTRAINT account_network_pending
    FOREIGN KEY(operator_id,account_id,pending_binding_id)
    REFERENCES account_network_bindings(operator_id,account_id,binding_id);
CREATE FUNCTION preserve_account_network_binding() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'account network bindings are immutable';
END;
$$;
CREATE TRIGGER account_network_binding_immutable BEFORE UPDATE OR DELETE ON account_network_bindings
    FOR EACH ROW EXECUTE FUNCTION preserve_account_network_binding();
ALTER TABLE browser_operations ADD COLUMN network_binding_id UUID;
ALTER TABLE browser_operations ADD COLUMN network_scope JSONB;
ALTER TABLE browser_operations ADD CONSTRAINT browser_operation_network_binding
    FOREIGN KEY(operator_id,account_id,network_binding_id)
    REFERENCES account_network_bindings(operator_id,account_id,binding_id);

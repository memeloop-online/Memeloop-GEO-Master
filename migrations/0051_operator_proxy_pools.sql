CREATE TABLE operator_proxy_pools (
    operator_id uuid NOT NULL REFERENCES operators(operator_id),
    pool_id uuid NOT NULL,
    version bigint NOT NULL CHECK (version > 0),
    payload jsonb NOT NULL,
    PRIMARY KEY(operator_id,pool_id)
);
CREATE TABLE operator_proxy_members (
    operator_id uuid NOT NULL,
    pool_id uuid NOT NULL,
    member_id uuid NOT NULL,
    version bigint NOT NULL CHECK (version > 0),
    revision_id uuid NOT NULL,
    payload jsonb NOT NULL,
    PRIMARY KEY(operator_id,pool_id,member_id),
    FOREIGN KEY(operator_id,pool_id) REFERENCES operator_proxy_pools(operator_id,pool_id)
);
CREATE TABLE operator_proxy_revisions (
    operator_id uuid NOT NULL,
    pool_id uuid NOT NULL,
    member_id uuid NOT NULL,
    revision_id uuid NOT NULL,
    encrypted_proxy bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(operator_id,pool_id,member_id,revision_id),
    FOREIGN KEY(operator_id,pool_id,member_id) REFERENCES operator_proxy_members(operator_id,pool_id,member_id)
);
ALTER TABLE operator_proxy_members ADD CONSTRAINT operator_proxy_current_revision
    FOREIGN KEY(operator_id,pool_id,member_id,revision_id)
    REFERENCES operator_proxy_revisions(operator_id,pool_id,member_id,revision_id)
    DEFERRABLE INITIALLY DEFERRED;
-- No UPDATE or DELETE operation is exposed by the repository for revisions.
CREATE FUNCTION preserve_operator_proxy_revision() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'proxy revisions are immutable';
END;
$$;
CREATE TRIGGER operator_proxy_revision_immutable
    BEFORE UPDATE OR DELETE ON operator_proxy_revisions
    FOR EACH ROW EXECUTE FUNCTION preserve_operator_proxy_revision();

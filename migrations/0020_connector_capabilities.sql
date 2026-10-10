-- Operator-owned capability settings are not proof of publication. A verified
-- connector requires independent, immutable live receipt and readback records.
CREATE TABLE connector_capability_settings (
    operator_id UUID NOT NULL REFERENCES operators(operator_id) ON DELETE RESTRICT,
    platform_id TEXT NOT NULL,
    placement_slot TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    content_types JSONB NOT NULL DEFAULT '[]'::jsonb
        CHECK (jsonb_typeof(content_types) = 'array'),
    PRIMARY KEY (operator_id,platform_id,placement_slot)
);

CREATE TABLE connector_capability_verifications (
    verification_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL REFERENCES operators(operator_id) ON DELETE RESTRICT,
    platform_id TEXT NOT NULL,
    placement_slot TEXT NOT NULL,
    connector_version TEXT NOT NULL,
    content_type TEXT NOT NULL,
    publication_receipt JSONB NOT NULL,
    public_readback JSONB NOT NULL,
    verified_at TIMESTAMPTZ NOT NULL,
    UNIQUE (operator_id,verification_id)
);
CREATE INDEX connector_verifications_lookup_idx
    ON connector_capability_verifications
        (operator_id,platform_id,placement_slot,connector_version,content_type,verified_at DESC);

-- Preserve historical evidence even if settings are revoked or a runner is upgraded.
CREATE FUNCTION forbid_connector_verification_mutation()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'connector verification records are immutable';
END;
$$;
CREATE TRIGGER connector_verification_immutable
    BEFORE UPDATE OR DELETE ON connector_capability_verifications
    FOR EACH ROW EXECUTE FUNCTION forbid_connector_verification_mutation();

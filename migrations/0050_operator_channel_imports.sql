-- Onboarding credentials are independent from verified browser sessions.
-- No unkeyed credential digest or plaintext input is persisted.
CREATE TABLE operator_channel_import_rows (
    operator_id uuid NOT NULL REFERENCES operators(operator_id),
    import_key text NOT NULL CHECK (length(import_key) BETWEEN 1 AND 128),
    row_key text NOT NULL CHECK (length(row_key) BETWEEN 1 AND 128),
    secret_id uuid NOT NULL,
    credential_version integer NOT NULL DEFAULT 1 CHECK (credential_version > 0),
    encrypted_input bytea NOT NULL,
    outcome jsonb NOT NULL,
    PRIMARY KEY (operator_id, import_key, row_key)
);

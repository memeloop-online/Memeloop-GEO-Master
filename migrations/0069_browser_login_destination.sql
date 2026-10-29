-- Keep the credential destination after a verified login so disposal retries
-- do not repeat identity checks or overwrite the saved session.
ALTER TABLE browser_operations ADD COLUMN login_verified_account_id UUID;
UPDATE browser_operations SET login_verified_account_id=account_id
    WHERE login_verified_at IS NOT NULL;
ALTER TABLE browser_operations ADD CONSTRAINT browser_login_destination_verified
    CHECK ((login_verified_at IS NULL) = (login_verified_account_id IS NULL));

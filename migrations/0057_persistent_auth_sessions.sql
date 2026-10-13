-- NULL explicitly denotes a revocable first-party session without a time limit.
-- Existing finite sessions retain their original expiry and revocation state.
ALTER TABLE sessions ALTER COLUMN expires_at DROP NOT NULL;

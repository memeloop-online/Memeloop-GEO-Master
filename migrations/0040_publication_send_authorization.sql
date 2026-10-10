-- One-shot permission on the ORIGINAL channel send attempt, not another send.
-- Registration stores only an ephemeral browser identity and a short deadline;
-- encrypted account/network provenance remains in publication_execution_bindings.
ALTER TABLE channel_execution_attempts
    ADD COLUMN runner_session_id UUID,
    ADD COLUMN send_not_after TIMESTAMPTZ,
    ADD COLUMN send_authorized_at TIMESTAMPTZ,
    ADD CONSTRAINT channel_attempt_send_registration_pair
        CHECK ((runner_session_id IS NULL) = (send_not_after IS NULL)),
    ADD CONSTRAINT channel_attempt_send_authorization_registration
        CHECK (send_authorized_at IS NULL
            OR (runner_session_id IS NOT NULL AND send_not_after IS NOT NULL
                AND target_kind = 'publish'
                AND send_authorized_at <= send_not_after));

-- Reject direct rewriting of an established identity, deadline, or grant.
-- First registration and first authorization are the only permitted changes.
CREATE FUNCTION guard_publication_send_authorization() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (OLD.runner_session_id IS NOT NULL AND
        (NEW.runner_session_id IS DISTINCT FROM OLD.runner_session_id
         OR NEW.send_not_after IS DISTINCT FROM OLD.send_not_after))
        OR (OLD.send_authorized_at IS NOT NULL AND
            NEW.send_authorized_at IS DISTINCT FROM OLD.send_authorized_at)
        OR (OLD.outcome IS NOT NULL AND
            (NEW.runner_session_id IS DISTINCT FROM OLD.runner_session_id
             OR NEW.send_authorized_at IS DISTINCT FROM OLD.send_authorized_at))
        OR (OLD.target_kind <> 'publish' AND
            (NEW.runner_session_id IS NOT NULL OR NEW.send_authorized_at IS NOT NULL))
    THEN
        RAISE EXCEPTION 'publication send registration and grant are immutable';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER channel_attempt_send_authorization_guard
    BEFORE UPDATE ON channel_execution_attempts
    FOR EACH ROW EXECUTE FUNCTION guard_publication_send_authorization();

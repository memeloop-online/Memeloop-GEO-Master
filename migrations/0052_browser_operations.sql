-- Expiry schedules reconciliation; only exact disposal proof releases ownership.
CREATE TABLE browser_operations (
    operation_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL REFERENCES operators(operator_id),
    account_id UUID NOT NULL,
    session_id UUID NOT NULL UNIQUE,
    reservation_id UUID NOT NULL,
    scope JSONB NOT NULL,
    purpose JSONB NOT NULL,
    runner_generation UUID NOT NULL,
    effective_network BYTEA NOT NULL,
    state JSONB NOT NULL,
    version BIGINT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    next_check_at TIMESTAMPTZ NOT NULL,
    released_unstarted BOOLEAN NOT NULL DEFAULT FALSE
);
CREATE INDEX browser_operations_due ON browser_operations(next_check_at,operation_id)
    WHERE state <> '"Disposed"'::jsonb;
ALTER TABLE channel_account_preflight_reservations
    ADD COLUMN browser_operation_id UUID REFERENCES browser_operations(operation_id);

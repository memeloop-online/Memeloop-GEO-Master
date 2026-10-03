-- Reversible account-wide browser preflight reservation. This is separate
-- from the irreversible one-shot external attempt ledger.
CREATE TABLE channel_account_preflight_reservations (
    operator_id UUID NOT NULL REFERENCES operators(operator_id) ON DELETE CASCADE,
    account_id UUID NOT NULL,
    reservation_id UUID NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (operator_id, account_id)
);

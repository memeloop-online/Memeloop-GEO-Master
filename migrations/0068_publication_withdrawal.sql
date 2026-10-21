-- Cleanup authority is accepted before sending and frozen with the exact attempt.
-- Historical requests did not authorize withdrawal.
ALTER TABLE content_distribution_requests
    ADD COLUMN withdraw_after_publish BOOLEAN NOT NULL DEFAULT FALSE,
    ADD CONSTRAINT content_request_withdrawal_shape CHECK (
        NOT withdraw_after_publish OR
        (account_owner_kind='customer' AND platform_id='zhihu' AND placement_slot='short_post' AND format='markdown.v1')
    );

CREATE TABLE publication_withdrawals (
    attempt_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    request_id UUID NOT NULL,
    target_id UUID NOT NULL,
    account_id UUID NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('awaiting_publication','ready','claimed','unknown','withdrawn')),
    execution_id UUID UNIQUE,
    claimed_at TIMESTAMPTZ,
    body JSONB NOT NULL,
    UNIQUE (operator_id,tenant_id,project_id,attempt_id),
    CHECK (body->>'state'=state),
    CHECK ((execution_id IS NULL) = (claimed_at IS NULL)),
    CHECK ((state IN ('awaiting_publication','ready')) = (execution_id IS NULL)),
    FOREIGN KEY (operator_id,tenant_id,project_id,request_id)
        REFERENCES content_distribution_requests (operator_id,tenant_id,project_id,request_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id,tenant_id,project_id,target_id,attempt_id,account_id)
        REFERENCES channel_execution_attempts (operator_id,tenant_id,project_id,target_id,attempt_id,account_id) ON DELETE RESTRICT
);
CREATE INDEX publication_withdrawals_scope_state_idx
    ON publication_withdrawals (operator_id,tenant_id,project_id,state,attempt_id);
CREATE INDEX publication_withdrawals_pending_idx
    ON publication_withdrawals (attempt_id)
    WHERE state IN ('awaiting_publication','ready','claimed');

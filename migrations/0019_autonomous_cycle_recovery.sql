-- A cycle bootstrap is an independently fenced, retryable operation. Its
-- attempt metadata is never part of the immutable cycle configuration.
ALTER TABLE optimization_cycles
    ADD COLUMN content_bootstrap_token UUID,
    ADD COLUMN content_bootstrap_expires_at TIMESTAMPTZ,
    ADD COLUMN content_bootstrap_retry_after TIMESTAMPTZ,
    ADD COLUMN content_bootstrap_attempts INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN content_bootstrap_error_code TEXT,
    ADD CONSTRAINT content_bootstrap_pair CHECK
        ((content_bootstrap_token IS NULL) = (content_bootstrap_expires_at IS NULL));
CREATE INDEX optimization_cycle_bootstrap_idx ON optimization_cycles (cycle_id)
    WHERE content_bootstrap_token IS NULL;
CREATE INDEX content_closed_dispatch_idx ON content_executions (execution_id)
    WHERE state->'execution'->>'status' = 'closed';

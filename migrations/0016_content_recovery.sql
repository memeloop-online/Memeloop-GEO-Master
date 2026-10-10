-- A workflow dispatch is a separate, short-lived fenced claim. Item steps
-- remain the authority for individual provider transformations and outputs.
ALTER TABLE content_executions
    ADD COLUMN dispatch_token UUID,
    ADD COLUMN dispatch_expires_at TIMESTAMPTZ,
    ADD COLUMN dispatch_retry_after TIMESTAMPTZ,
    ADD CONSTRAINT content_dispatch_pair CHECK
        ((dispatch_token IS NULL) = (dispatch_expires_at IS NULL));
CREATE INDEX content_running_dispatch_idx ON content_executions (execution_id)
    WHERE state->'execution'->>'status' = 'running';

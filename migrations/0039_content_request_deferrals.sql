-- Mutable, allowlisted recovery diagnostics; accepted request identity and
-- the original publication outbox remain unchanged.
ALTER TABLE content_distribution_requests
    ADD COLUMN materialization_reason TEXT,
    ADD COLUMN materialization_attempts INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN materialization_next_retry_at TIMESTAMPTZ,
    ADD CONSTRAINT content_request_deferral_reason_check
        CHECK (materialization_reason IS NULL OR materialization_reason IN (
            'project_paused', 'account_unavailable', 'connector_unavailable',
            'content_not_ready', 'source_unavailable', 'format_unsupported',
            'temporary_failure', 'internal_error'
        )),
    ADD CONSTRAINT content_request_deferral_state_check
        CHECK (
            materialization_attempts >= 0
            AND ((materialization_reason IS NULL AND materialization_next_retry_at IS NULL
                  AND materialization_attempts = 0)
              OR (materialization_reason IS NOT NULL AND materialization_next_retry_at IS NOT NULL
                  AND materialization_attempts > 0 AND publication_intent_id IS NULL))
        );

-- No clock-dependent partial-index predicate: the scanner applies the due
-- timestamp at query time and retains request-id keyset pagination.
CREATE INDEX content_request_deferral_due_idx
    ON content_distribution_requests (materialization_next_retry_at, request_id)
    WHERE publication_intent_id IS NULL;

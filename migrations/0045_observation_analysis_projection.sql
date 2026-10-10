-- Projection reads choose one grounded revision per attempt without walking
-- newer failed/unverified history. Existing observations and reports stay intact.
CREATE INDEX observation_analyses_grounded_projection
    ON observation_analyses (
        operator_id, tenant_id, project_id, target_id, attempt_id,
        created_at DESC, revision_id DESC
    )
    WHERE state = 'completed' AND result->'outcome'->>'status' = 'grounded';

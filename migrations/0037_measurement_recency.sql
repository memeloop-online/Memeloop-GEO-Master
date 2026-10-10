-- Stable newest-first pages for history and citation insights. The cursor
-- still identifies an immutable plan inside the same project.
CREATE INDEX measurement_execution_plans_recency_idx
    ON measurement_execution_plans
    (operator_id, tenant_id, project_id, created_at DESC, plan_id DESC);

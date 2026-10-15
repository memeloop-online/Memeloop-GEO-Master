-- Independent reports share project settings but never require a cycle.
CREATE TABLE measurement_weekly_schedules (
    schedule_id UUID PRIMARY KEY,
    sequence BIGSERIAL NOT NULL UNIQUE,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    report_window_start_at TIMESTAMPTZ NOT NULL,
    report_window_end_at TIMESTAMPTZ NOT NULL,
    report_timezone TEXT NOT NULL,
    evidence_as_of TIMESTAMPTZ NOT NULL,
    report_at TIMESTAMPTZ NOT NULL,
    due_at TIMESTAMPTZ NOT NULL,
    report_schedule JSONB NOT NULL,
    report_id UUID,
    completed_at TIMESTAMPTZ,
    FOREIGN KEY (operator_id,tenant_id,project_id)
        REFERENCES projects (operator_id,tenant_id,project_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id,tenant_id,project_id,report_id)
        REFERENCES measurement_period_reports (operator_id,tenant_id,project_id,report_id) ON DELETE RESTRICT,
    UNIQUE (operator_id,tenant_id,project_id,report_window_start_at,report_window_end_at,report_timezone),
    CHECK (report_window_start_at < report_window_end_at),
    CHECK (report_window_end_at <= evidence_as_of AND report_window_end_at <= report_at),
    CHECK (due_at = GREATEST(report_at,evidence_as_of)),
    CHECK ((report_id IS NULL) = (completed_at IS NULL)),
    CHECK (completed_at IS NULL OR completed_at >= due_at)
);
CREATE UNIQUE INDEX measurement_weekly_one_pending_project_idx
    ON measurement_weekly_schedules (operator_id,tenant_id,project_id) WHERE report_id IS NULL;
CREATE INDEX measurement_weekly_pending_due_idx
    ON measurement_weekly_schedules (due_at,schedule_id) WHERE report_id IS NULL;
CREATE INDEX measurement_weekly_project_sequence_idx
    ON measurement_weekly_schedules (operator_id,tenant_id,project_id,sequence DESC);

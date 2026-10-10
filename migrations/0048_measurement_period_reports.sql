-- Independent topic reports need only a project, never an optimization cycle.
CREATE TABLE measurement_period_reports (
    report_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    correction_of UUID,
    report_window_start_at TIMESTAMPTZ NOT NULL,
    report_window_end_at TIMESTAMPTZ NOT NULL,
    report_timezone TEXT NOT NULL,
    evidence_as_of TIMESTAMPTZ NOT NULL,
    snapshot JSONB NOT NULL,
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    UNIQUE (operator_id, tenant_id, project_id, report_id),
    FOREIGN KEY (operator_id, tenant_id, project_id, correction_of)
        REFERENCES measurement_period_reports (operator_id, tenant_id, project_id, report_id) ON DELETE RESTRICT,
    CHECK (report_window_start_at < report_window_end_at),
    CHECK (report_window_end_at <= evidence_as_of),
    CHECK ((revision = 1 AND correction_of IS NULL) OR (revision > 1 AND correction_of IS NOT NULL)),
    UNIQUE (operator_id, tenant_id, project_id, report_window_start_at,
        report_window_end_at, report_timezone, revision)
);
CREATE INDEX measurement_period_reports_project_window_idx
    ON measurement_period_reports (operator_id, tenant_id, project_id, report_window_end_at DESC, revision DESC);

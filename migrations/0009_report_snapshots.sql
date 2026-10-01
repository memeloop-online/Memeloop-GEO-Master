-- Append-only weekly report snapshots. Source material remains in its own
-- aggregates; this table freezes the reducer's scoped input and output.
CREATE TABLE report_snapshots (
    report_id UUID PRIMARY KEY,
    operator_id UUID NOT NULL,
    tenant_id UUID NOT NULL,
    project_id UUID NOT NULL,
    cycle_id UUID NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    correction_of UUID,
    report_window_start_at TIMESTAMPTZ NOT NULL,
    report_window_end_at TIMESTAMPTZ NOT NULL,
    cutoff_at TIMESTAMPTZ NOT NULL,
    reducer_version TEXT NOT NULL,
    input_manifest_versions JSONB NOT NULL,
    input_hash TEXT NOT NULL,
    snapshot JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (operator_id, tenant_id, project_id)
        REFERENCES projects (operator_id, tenant_id, project_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, cycle_id)
        REFERENCES optimization_cycles (operator_id, tenant_id, project_id, cycle_id) ON DELETE RESTRICT,
    FOREIGN KEY (operator_id, tenant_id, project_id, correction_of)
        REFERENCES report_snapshots (operator_id, tenant_id, project_id, report_id) ON DELETE RESTRICT,
    CHECK (report_window_start_at < report_window_end_at),
    CHECK (cutoff_at >= report_window_end_at),
    CHECK ((revision = 1 AND correction_of IS NULL) OR (revision > 1 AND correction_of IS NOT NULL)),
    UNIQUE (operator_id, tenant_id, project_id, report_id),
    UNIQUE (operator_id, tenant_id, project_id, cycle_id, report_window_start_at,
        report_window_end_at, cutoff_at, reducer_version, input_manifest_versions, revision)
);
CREATE INDEX report_snapshots_project_window_idx
    ON report_snapshots (operator_id, tenant_id, project_id, report_window_end_at DESC, revision DESC);

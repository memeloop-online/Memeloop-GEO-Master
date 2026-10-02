-- One successor per predecessor and natural-week window, scoped to project.
-- Existing first-start rows retain a NULL predecessor.
ALTER TABLE optimization_cycles
    ADD COLUMN previous_cycle_id UUID,
    ADD CONSTRAINT optimization_cycles_previous_scope_fk
        FOREIGN KEY (operator_id, tenant_id, project_id, previous_cycle_id)
        REFERENCES optimization_cycles (operator_id, tenant_id, project_id, cycle_id)
        ON DELETE RESTRICT,
    ADD CONSTRAINT optimization_cycles_window_order_check
        CHECK (report_window_start_at < report_window_end_at),
    ADD CONSTRAINT optimization_cycles_cutoff_order_check
        CHECK (cutoff_at >= report_window_end_at) NOT VALID;

CREATE UNIQUE INDEX optimization_cycles_predecessor_unique
    ON optimization_cycles (operator_id, tenant_id, project_id, previous_cycle_id)
    WHERE previous_cycle_id IS NOT NULL;

CREATE UNIQUE INDEX optimization_cycles_window_unique
    ON optimization_cycles (operator_id, tenant_id, project_id,
                            report_window_start_at, report_window_end_at);

CREATE INDEX optimization_cycles_pending_successor_idx
    ON optimization_cycles (cutoff_at, cycle_id);

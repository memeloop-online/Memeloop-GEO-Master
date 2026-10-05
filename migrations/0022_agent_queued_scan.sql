-- Bounded keyset discovery for trusted background dispatch. Discovery never
-- claims work; the scoped queued-to-running update remains the sole claim.
CREATE INDEX agent_runs_queued_scan_idx
    ON agent_runs (created_at, run_id)
    WHERE status = 'queued';

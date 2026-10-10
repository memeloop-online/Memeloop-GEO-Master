-- Report fan-in starts from a bounded set of frozen original target IDs.
-- Keep that lookup independent of the project's total job/history count.
CREATE INDEX publication_lookup_jobs_report_target_idx
    ON publication_lookup_jobs (operator_id, tenant_id, project_id, target_id);

CREATE INDEX publication_lookup_observations_report_asset_idx
    ON publication_lookup_observations
        (operator_id, tenant_id, project_id, attempt_id, received_at DESC, execution_id DESC)
    WHERE finding = 'asset_observed';

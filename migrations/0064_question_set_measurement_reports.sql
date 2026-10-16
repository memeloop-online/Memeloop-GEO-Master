-- A question-set version is an independent immutable report series.
ALTER TABLE measurement_period_reports
    ADD COLUMN question_set_id UUID,
    ADD COLUMN question_set_version_id UUID,
    ADD CONSTRAINT measurement_period_question_set_pair
        CHECK ((question_set_id IS NULL) = (question_set_version_id IS NULL)),
    ADD CONSTRAINT measurement_period_question_set_version_fk
        FOREIGN KEY (operator_id,tenant_id,project_id,question_set_id,question_set_version_id)
        REFERENCES question_set_versions
            (operator_id,tenant_id,project_id,question_set_id,question_set_version_id)
        ON DELETE RESTRICT;

-- Resolve PostgreSQL's generated constraint name through its exact columns.
DO $$
DECLARE window_constraint TEXT;
BEGIN
    SELECT c.conname INTO STRICT window_constraint
    FROM pg_constraint c
    WHERE c.conrelid = 'measurement_period_reports'::regclass AND c.contype = 'u'
      AND (SELECT array_agg(a.attname::TEXT ORDER BY k.position)
           FROM unnest(c.conkey) WITH ORDINALITY AS k(attnum,position)
           JOIN pg_attribute a ON a.attrelid=c.conrelid AND a.attnum=k.attnum)
          = ARRAY['operator_id','tenant_id','project_id','report_window_start_at',
                  'report_window_end_at','report_timezone','revision'];
    EXECUTE format('ALTER TABLE measurement_period_reports DROP CONSTRAINT %I', window_constraint);
END $$;

CREATE UNIQUE INDEX measurement_period_project_revision_idx
    ON measurement_period_reports
        (operator_id,tenant_id,project_id,report_window_start_at,
         report_window_end_at,report_timezone,revision)
    WHERE question_set_id IS NULL;
CREATE UNIQUE INDEX measurement_period_question_set_revision_idx
    ON measurement_period_reports
        (operator_id,tenant_id,project_id,question_set_id,question_set_version_id,
         report_window_start_at,report_window_end_at,report_timezone,revision)
    WHERE question_set_id IS NOT NULL;

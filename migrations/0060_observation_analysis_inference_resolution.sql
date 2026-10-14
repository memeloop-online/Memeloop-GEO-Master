-- Terminal local failure alone cannot establish that remote inference ended.
ALTER TABLE observation_analyses
    ADD COLUMN inference_resolved BOOLEAN NOT NULL DEFAULT FALSE;

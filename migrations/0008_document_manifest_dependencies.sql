-- Exact public source versions used by a first-stage document branch.
-- This is a conservative project-level dependency set until product/fact
-- extraction can narrow each branch without dropping coverage.
ALTER TABLE document_manifest_items
    ADD COLUMN source_version_refs JSONB NOT NULL DEFAULT '[]'::JSONB;

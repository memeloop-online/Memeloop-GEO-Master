-- Direct-child lookup bounds JSON decoding to revisions of one asset and
-- base; both historical base_revision_id and copy-on-write provenance are
-- indexed in revision order for keyset pagination.
CREATE INDEX content_revisions_base_lookup_idx ON content_revisions
    (operator_id, tenant_id, project_id, asset_id, (body->>'base_revision_id'), revision);
CREATE INDEX content_revisions_derived_lookup_idx ON content_revisions
    (operator_id, tenant_id, project_id, asset_id, derived_from_revision_id, revision);

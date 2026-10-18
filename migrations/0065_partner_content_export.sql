-- Existing inbound connections gain no authority to download generated content.
ALTER TABLE partner_connections ADD COLUMN content_export BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE partner_audit_events DROP CONSTRAINT partner_audit_events_event_check;
ALTER TABLE partner_audit_events ADD CONSTRAINT partner_audit_events_event_check
    CHECK (event IN ('connection_created','connection_revoked','key_issued','key_revoked',
                    'content_export_enabled','content_export_disabled'));

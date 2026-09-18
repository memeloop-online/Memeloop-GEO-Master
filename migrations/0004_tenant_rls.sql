-- RLS is intentionally deferred until every PostgreSQL repository sets the
-- transaction-local geo.operator_id / geo.tenant_id / geo.project_id values.
-- Applying FORCE ROW LEVEL SECURITY before that wiring would make the current
-- schema unusable (including idempotency writes), so this compatibility
-- migration is a deliberate no-op.  Keep the policy design in the worklog
-- and enable it only together with repository transaction-local scope setup.
SELECT 1;

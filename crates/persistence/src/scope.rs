use geo_domain::TenantScope;
use sqlx::{Postgres, Transaction};

/// Set the transaction-local scope expected by the deferred RLS policy.
/// Keeping this in one helper prevents repositories from accidentally using
/// session-global settings that could leak across pooled connections.
pub async fn set_local_scope(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT set_config('geo.operator_id', $1, true), set_config('geo.tenant_id', $2, true), set_config('geo.project_id', $3, true)")
        .bind(scope.operator_id.to_string())
        .bind(scope.tenant_id.to_string())
        .bind(
            scope
                .project_id
                .map(|project_id| project_id.to_string())
                .unwrap_or_default(),
        )
        .execute(&mut **transaction)
        .await
        .map(|_| ())
}

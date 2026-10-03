//! Real PostgreSQL contract; run with a disposable GEO_TEST_DATABASE_URL.

use geo_domain::TenantScope;
use geo_persistence::{Database, DatabaseConfig, PgModelRouteRepository};
use sqlx::PgPool;
use uuid::Uuid;

async fn identity(pool: &PgPool) -> (Uuid, Uuid, Uuid) {
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    let project = Uuid::new_v4();
    sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,'test')")
        .bind(operator)
        .bind(format!("route-test-{operator}"))
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tenants(operator_id,tenant_id,slug,display_name) VALUES($1,$2,$3,'test')",
    )
    .bind(operator)
    .bind(tenant)
    .bind(format!("route-test-{tenant}"))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO projects(operator_id,tenant_id,project_id,slug,display_name) VALUES($1,$2,$3,$4,'test')")
        .bind(operator).bind(tenant).bind(project).bind(format!("route-test-{project}"))
        .execute(pool).await.unwrap();
    (operator, tenant, project)
}

async fn grant(
    pool: &PgPool,
    operator: Uuid,
    tenant: Uuid,
    project: Option<Uuid>,
    model: &str,
    is_default: bool,
    enabled: bool,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tenant_model_routes(route_id,operator_id,tenant_id,project_id,model,\
         is_default,enabled,tenant_external_id,principal_external_id,key_id,credential_generation)\
         VALUES($1,$2,$3,$4,$5,$6,$7,'test-tenant','test-subject',$8,2)",
    )
    .bind(id)
    .bind(operator)
    .bind(tenant)
    .bind(project)
    .bind(model)
    .bind(is_default)
    .bind(enabled)
    .bind(Uuid::new_v4())
    .execute(pool)
    .await
    .unwrap();
    id
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL in GEO_TEST_DATABASE_URL"]
async fn grants_are_scoped_uncached_and_project_revocation_shadows_tenant() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable test database");
    let config = DatabaseConfig::from_url(url).unwrap();
    let db = Database::connect_and_migrate(&config).await.unwrap();
    let pool = db.pool();
    let (operator, tenant, project) = identity(pool).await;
    let other_tenant = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tenants(operator_id,tenant_id,slug,display_name) VALUES($1,$2,$3,'test')",
    )
    .bind(operator)
    .bind(other_tenant)
    .bind(format!("route-test-{other_tenant}"))
    .execute(pool)
    .await
    .unwrap();
    let tenant_route = grant(pool, operator, tenant, None, "model-a", true, true).await;
    let project_route = grant(
        pool,
        operator,
        tenant,
        Some(project),
        "model-a",
        true,
        false,
    )
    .await;
    let routes = PgModelRouteRepository::from_database(&db);
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.into()));
    let tenant_scope = TenantScope::new(operator.into(), tenant.into(), None);
    let other_scope = TenantScope::new(operator.into(), other_tenant.into(), None);

    assert_eq!(
        routes
            .resolve(&scope, None)
            .await
            .unwrap()
            .unwrap()
            .route_id,
        project_route
    );
    assert!(
        !routes
            .resolve(&scope, Some("model-a"))
            .await
            .unwrap()
            .unwrap()
            .enabled
    );
    assert_eq!(
        routes
            .resolve(&tenant_scope, None)
            .await
            .unwrap()
            .unwrap()
            .route_id,
        tenant_route
    );
    assert!(
        routes
            .resolve(&other_scope, Some("model-a"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        routes
            .resolve(&tenant_scope, Some("unmapped"))
            .await
            .unwrap()
            .is_none()
    );

    sqlx::query(
        "UPDATE tenant_model_routes SET enabled=true,credential_generation=3 WHERE route_id=$1",
    )
    .bind(project_route)
    .execute(pool)
    .await
    .unwrap();
    let updated = routes.current(project_route).await.unwrap().unwrap();
    assert!(updated.enabled);
    assert_eq!(updated.credential_generation, 3);
    sqlx::query("UPDATE tenant_model_routes SET enabled=false WHERE route_id=$1")
        .bind(project_route)
        .execute(pool)
        .await
        .unwrap();
    assert!(
        !routes
            .current(project_route)
            .await
            .unwrap()
            .unwrap()
            .enabled
    );
}

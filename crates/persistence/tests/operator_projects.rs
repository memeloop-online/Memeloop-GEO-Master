use geo_domain::{ProjectCreate, ProjectRepository, ProjectSettings, TenantScope};
use geo_persistence::{Database, DatabaseConfig, PgProjectRepository};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn operator_project_directory_is_bounded_literal_and_isolated() {
    let config = DatabaseConfig::from_url(
        std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database required"),
    )
    .unwrap();
    let database = Database::connect_and_migrate(&config).await.unwrap();
    let repository = PgProjectRepository::from_database(&database);
    let operator = Uuid::new_v4();
    let foreign = Uuid::new_v4();
    let mut expected = Vec::new();
    for owner in [operator, foreign] {
        sqlx::query("INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,$3)")
            .bind(owner)
            .bind(format!("directory-{owner}"))
            .bind("Directory operator")
            .execute(database.pool())
            .await
            .unwrap();
        for index in 0..3 {
            let tenant = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO tenants(tenant_id,operator_id,slug,display_name) VALUES($1,$2,$3,$4)",
            )
            .bind(tenant)
            .bind(owner)
            .bind(format!("directory-{tenant}"))
            .bind(format!("Directory tenant {index}"))
            .execute(database.pool())
            .await
            .unwrap();
            let project = repository
                .create(
                    &TenantScope::new(owner.into(), tenant.into(), None),
                    ProjectCreate {
                        slug: Some(format!("directory-{index}")),
                        display_name: format!("Project {index} %_"),
                        settings: ProjectSettings::default(),
                    },
                )
                .await
                .unwrap();
            if owner == operator {
                expected.push(project.id);
            }
        }
    }
    expected.sort();
    let first = repository
        .list_operator_page(operator.into(), "", 2, None)
        .await
        .unwrap();
    assert_eq!(
        first
            .items
            .iter()
            .map(|item| item.project_id)
            .collect::<Vec<_>>(),
        expected[..2]
    );
    assert!(
        first
            .items
            .iter()
            .all(|item| item.tenant_display_name.is_some())
    );
    let second = repository
        .list_operator_page(
            operator.into(),
            "",
            2,
            Some(Uuid::parse_str(first.next_cursor.as_ref().unwrap()).unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].project_id, expected[2]);
    assert!(second.next_cursor.is_none());
    for search in ["PROJECT 1", "directory-1", "  PROJECT 1  "] {
        let page = repository
            .list_operator_page(operator.into(), search, 25, None)
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].display_name, "Project 1 %_");
    }
    let literal = repository
        .list_operator_page(operator.into(), "%_", 25, None)
        .await
        .unwrap();
    assert_eq!(literal.items.len(), 3);
    let absent = repository
        .list_operator_page(operator.into(), "Project %", 25, None)
        .await
        .unwrap();
    assert!(absent.items.is_empty());
    assert!(
        repository
            .list_operator_page(operator.into(), "", 0, None)
            .await
            .is_err()
    );
    assert!(
        repository
            .list_operator_page(operator.into(), &"x".repeat(201), 25, None)
            .await
            .is_err()
    );
}

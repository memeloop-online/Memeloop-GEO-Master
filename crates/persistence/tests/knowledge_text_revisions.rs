use geo_domain::{
    ImportItem, KnowledgePurpose, KnowledgeRepository, ProjectCreate, ProjectRepository,
    ProjectSettings, ReviseSourceTextCommand, SourceKind, SourceTextBasis, TenantScope, sha256_hex,
};
use geo_persistence::{Database, DatabaseConfig, PgKnowledgeRepository, PgProjectRepository};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
async fn scoped_authored_revision_replays_after_repository_restart() {
    let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database");
    let database = Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
        .await
        .unwrap();
    let operator = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    sqlx::query("INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Synthetic')")
        .bind(operator)
        .bind(format!("revision-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Synthetic')")
        .bind(tenant).bind(operator).bind(format!("revision-{tenant}"))
        .execute(database.pool()).await.unwrap();
    let tenant_scope = TenantScope::new(operator.into(), tenant.into(), None);
    let project = PgProjectRepository::from_database(&database)
        .create(
            &tenant_scope,
            ProjectCreate {
                slug: Some(format!("revision-{tenant}")),
                display_name: "Synthetic".to_owned(),
                settings: ProjectSettings {
                    brand_name: "Synthetic".to_owned(),
                    market: "US".to_owned(),
                    language: "en".to_owned(),
                    ..ProjectSettings::default()
                },
            },
        )
        .await
        .unwrap();
    let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.id));
    let repository = PgKnowledgeRepository::from_database(&database);
    let import = repository
        .import_batch(
            &scope,
            vec![ImportItem {
                client_item_id: format!("revision-{}", Uuid::new_v4()),
                kind: SourceKind::Text,
                name: "Synthetic".to_owned(),
                purpose: KnowledgePurpose::Public,
                text: Some("Original evidence".to_owned()),
                url: None,
                object_id: None,
                knowledge_release_id: None,
            }],
        )
        .await
        .unwrap()
        .items
        .remove(0);
    let source = import.source.unwrap();
    let original = import.source_version.unwrap();
    let command = ReviseSourceTextCommand {
        base_version_id: original.source_version_id,
        media_type: "text/markdown".to_owned(),
        text: "  中文\n\n* evidence  \n".to_owned(),
    };
    let receipt = repository
        .revise_source_text(
            &scope,
            source.source_id,
            source.revision,
            "stable-key",
            command.clone(),
        )
        .await
        .unwrap();
    let restarted = PgKnowledgeRepository::from_database(&database);
    assert_eq!(
        restarted
            .revise_source_text(
                &scope,
                source.source_id,
                source.revision,
                "stable-key",
                command.clone(),
            )
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(
        restarted
            .get_source_version_content(
                &scope,
                source.source_id,
                receipt.source_version.source_version_id,
            )
            .await
            .unwrap()
            .unwrap()
            .text,
        command.text
    );
    assert!(sqlx::query(
        "UPDATE knowledge_authored_text SET body='changed' WHERE source_version_id=$1",
    )
    .bind(receipt.source_version.source_version_id)
    .execute(database.pool())
    .await
    .is_err());
    assert!(
        sqlx::query(
            "INSERT INTO knowledge_authored_text
         (operator_id,tenant_id,project_id,source_id,source_version_id,media_type,body)
         VALUES ($1,$2,$3,$4,$5,'text/plain','incorrect representation')",
        )
        .bind(operator)
        .bind(tenant)
        .bind(project.id.as_uuid())
        .bind(source.source_id)
        .bind(original.source_version_id)
        .execute(database.pool())
        .await
        .is_err()
    );
    assert_eq!(
        restarted
            .get_source_version_content(&scope, source.source_id, original.source_version_id,)
            .await
            .unwrap()
            .unwrap()
            .text_basis,
        SourceTextBasis::Extracted
    );
    let detail = restarted
        .get_source_detail(&scope, source.source_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(detail.versions.len(), 2);
    assert!(
        detail
            .chunks
            .iter()
            .any(|chunk| chunk.source_version_id == original.source_version_id)
    );
    assert!(
        detail
            .chunks
            .iter()
            .any(|chunk| chunk.source_version_id == receipt.source_version.source_version_id)
    );
    assert_eq!(
        restarted
            .revise_source_text(
                &scope,
                source.source_id,
                source.revision,
                "stable-key",
                ReviseSourceTextCommand {
                    text: "Different".to_owned(),
                    ..command.clone()
                }
            )
            .await
            .unwrap_err()
            .details
            .unwrap()["reason"],
        "idempotency_conflict"
    );
    assert_eq!(
        restarted
            .revise_source_text(
                &scope,
                source.source_id,
                source.revision,
                "different-key",
                command,
            )
            .await
            .unwrap_err()
            .details
            .unwrap()["reason"],
        "source_revision_conflict"
    );
    assert!(
        restarted
            .get_source_version_content(&scope, Uuid::new_v4(), original.source_version_id)
            .await
            .unwrap()
            .is_none()
    );
    // An unsuccessful parse can reserve a number above the published pointer.
    let reserved_version_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO knowledge_source_versions
         (source_version_id,operator_id,tenant_id,project_id,source_id,version,
          content_sha256,captured_at,parent_version_id,parser_version,extraction_version)
         VALUES ($1,$2,$3,$4,$5,3,$6,now(),$7,'synthetic-test-v1','none-v1')",
    )
    .bind(reserved_version_id)
    .bind(operator)
    .bind(tenant)
    .bind(project.id.as_uuid())
    .bind(source.source_id)
    .bind(sha256_hex(b"unpublished"))
    .bind(receipt.source_version.source_version_id)
    .execute(database.pool())
    .await
    .unwrap();
    let next = ReviseSourceTextCommand {
        base_version_id: receipt.source_version.source_version_id,
        media_type: "text/plain".to_owned(),
        text: "A second branch".to_owned(),
    };
    let other = ReviseSourceTextCommand {
        text: "Competing second branch".to_owned(),
        ..next.clone()
    };
    let (left, right) = tokio::join!(
        restarted.revise_source_text(
            &scope,
            source.source_id,
            receipt.source.revision,
            "parallel-left",
            next,
        ),
        repository.revise_source_text(
            &scope,
            source.source_id,
            receipt.source.revision,
            "parallel-right",
            other,
        ),
    );
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    let losing = left.err().or(right.err()).unwrap();
    assert_eq!(
        losing.details.unwrap()["reason"],
        "source_revision_conflict"
    );
    let latest = restarted
        .get_source_detail(&scope, source.source_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.versions.len(), 4);
    assert_eq!(latest.versions.last().unwrap().version, 4);
    assert_eq!(latest.source.revision, source.revision + 2);
}

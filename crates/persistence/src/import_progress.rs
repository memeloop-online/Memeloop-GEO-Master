//! Scoped, read-only import receipts for durable workbench continuation.
//! The receipt identifies the original attempt, but its embedded acceptance
//! is never treated as the live status or as proof of a published release.

use geo_domain::{
    AppError, ImportAcceptance, ImportItem, ImportStage, ImportStatus, KnowledgeImportProgress,
    KnowledgePurpose, TenantScope, knowledge_import_progress_app_error,
    knowledge_import_progress_errors, sha256_hex,
};
use serde_json::Value;
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use super::{PgKnowledgeRepository, database_error, purpose, serialization_error};
use crate::set_local_scope;

async fn snapshot<'a>(
    repo: &'a PgKnowledgeRepository,
    scope: &TenantScope,
) -> Result<Transaction<'a, Postgres>, AppError> {
    let mut tx = repo.pool.begin().await.map_err(database_error)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    set_local_scope(&mut tx, scope)
        .await
        .map_err(database_error)?;
    Ok(tx)
}

pub(super) async fn get(
    repo: &PgKnowledgeRepository,
    scope: &TenantScope,
    job_id: Uuid,
    requested_purpose: KnowledgePurpose,
) -> Result<Option<KnowledgeImportProgress>, AppError> {
    let mut tx = snapshot(repo, scope).await?;
    let result = load(&mut tx, scope, job_id, requested_purpose, None, None).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

pub(super) async fn resolve(
    repo: &PgKnowledgeRepository,
    scope: &TenantScope,
    expected: &ImportItem,
) -> Result<Option<KnowledgeImportProgress>, AppError> {
    let project = PgKnowledgeRepository::project_id(scope)?;
    if expected.client_item_id.trim().is_empty() {
        return Err(AppError::invalid_request("client_item_id is required"));
    }
    let expected_hash = sha256_hex(&serde_json::to_vec(expected).map_err(serialization_error)?);
    let mut tx = snapshot(repo, scope).await?;
    let receipt = sqlx::query(
        "SELECT request_hash,acceptance FROM knowledge_import_receipts
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3
           AND action='import_item' AND client_item_id=$4",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(&expected.client_item_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?;
    let Some(receipt) = receipt else {
        tx.commit().await.map_err(database_error)?;
        return Ok(None);
    };
    if receipt.get::<String, _>("request_hash") != expected_hash {
        return Err(AppError::conflict(
            "client_item_id was already used with different input",
        ));
    }
    let acceptance: ImportAcceptance = serde_json::from_value(receipt.get("acceptance"))
        .map_err(|_| AppError::conflict("import receipt has an invalid persisted acceptance"))?;
    if acceptance.client_item_id != expected.client_item_id {
        return Err(AppError::conflict(
            "import receipt is bound to a different item",
        ));
    }
    let result = if let Some(job) = acceptance.import_job.as_ref() {
        if job.operator_id != scope.operator_id
            || job.tenant_id != scope.tenant_id
            || job.project_id != project
            || acceptance.source.as_ref().map(|s| s.source_id) != Some(job.source_id)
            || acceptance.operation.as_ref().map(|o| o.id) != Some(job.operation_id)
            || acceptance
                .source_version
                .as_ref()
                .map(|v| v.source_version_id)
                != job.source_version_id
        {
            return Err(AppError::conflict(
                "import receipt has an invalid job binding",
            ));
        }
        let progress = load(
            &mut tx,
            scope,
            job.import_job_id,
            expected.purpose,
            Some(expected),
            Some(&acceptance),
        )
        .await?;
        if progress.is_none() {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM knowledge_import_jobs
                 WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND import_job_id=$4)",
            )
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project.as_uuid())
            .bind(job.import_job_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?;
            if !exists {
                return Err(AppError::conflict("import receipt job is missing"));
            }
        }
        progress
    } else {
        if acceptance.source.is_some()
            || acceptance.source_version.is_some()
            || acceptance.release.is_some()
            || acceptance.operation.is_some()
            || acceptance.status != ImportStatus::Failed
        {
            return Err(AppError::conflict(
                "import receipt has an invalid failure binding",
            ));
        }
        let errors = acceptance.error.as_ref().map_or_else(Vec::new, |error| {
            vec![knowledge_import_progress_app_error(error)]
        });
        Some(KnowledgeImportProgress {
            import_job_id: None,
            status: ImportStatus::Failed,
            stage: None,
            source_id: None,
            source_version_id: None,
            knowledge_release_id: None,
            completed_units: 0,
            failed_units: 0,
            error_count: errors.len() as u32,
            errors,
        })
    };
    tx.commit().await.map_err(database_error)?;
    Ok(result)
}

async fn load(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    job_id: Uuid,
    requested_purpose: KnowledgePurpose,
    expected: Option<&ImportItem>,
    receipt: Option<&ImportAcceptance>,
) -> Result<Option<KnowledgeImportProgress>, AppError> {
    let project = PgKnowledgeRepository::project_id(scope)?;
    let row = sqlx::query(
        "SELECT job.import_job_id,job.operation_id,job.source_id,job.source_version_id,
                job.stage,job.status,job.input_hash,job.completed_units,job.failed_units,job.errors,
                source.source_id AS actual_source_id,source.kind AS source_kind,
                source.name AS source_name,source.state AS source_state,source.purpose,
                source.locator,version.source_version_id AS actual_version_id,
                version.source_id AS version_source_id,version.object_id AS version_object_id,
                version.object_version AS version_object_version,
                version.content_sha256 AS version_hash,
                task.import_job_id AS task_id,task.source_id AS task_source_id,
                task.source_version_id AS task_version_id,task.object_id AS task_object_id,
                task.object_version AS task_object_version,task.input_sha256 AS task_hash,
                task.released_id,
                op.operation_id AS actual_operation_id,op.kind AS operation_kind,
                op.status AS operation_status,op.result AS operation_result,
                obj.object_id AS actual_object_id,obj.object_version AS actual_object_version,
                obj.sha256 AS object_hash,obj.state AS object_state,
                obj.detected_media_type AS object_media_type
         FROM knowledge_import_jobs job
         LEFT JOIN knowledge_sources source
           ON source.operator_id=job.operator_id AND source.tenant_id=job.tenant_id
          AND source.project_id=job.project_id AND source.source_id=job.source_id
         LEFT JOIN knowledge_source_versions version
           ON version.operator_id=job.operator_id AND version.tenant_id=job.tenant_id
          AND version.project_id=job.project_id AND version.source_version_id=job.source_version_id
         LEFT JOIN knowledge_pdf_parse_tasks task
           ON task.operator_id=job.operator_id AND task.tenant_id=job.tenant_id
          AND task.project_id=job.project_id AND task.import_job_id=job.import_job_id
         LEFT JOIN operations op
           ON op.operator_id=job.operator_id AND op.tenant_id=job.tenant_id
          AND op.project_id=job.project_id AND op.operation_id=job.operation_id
         LEFT JOIN knowledge_stored_objects obj
           ON obj.operator_id=job.operator_id AND obj.tenant_id=job.tenant_id
          AND obj.project_id=job.project_id AND obj.object_id=version.object_id
         WHERE job.operator_id=$1 AND job.tenant_id=$2 AND job.project_id=$3
           AND job.import_job_id=$4",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(job_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    let Some(row) = row else {
        return Ok(None);
    };
    // A revoked or wrong-purpose source never becomes a continuation result.
    if row.get::<Option<Uuid>, _>("actual_source_id").is_none()
        || row.get::<String, _>("source_state") != "active"
        || (requested_purpose == KnowledgePurpose::Public
            && row.get::<String, _>("purpose") != purpose(KnowledgePurpose::Public))
        || expected.is_some_and(|item| row.get::<String, _>("purpose") != purpose(item.purpose))
    {
        return Ok(None);
    }
    let broken = || AppError::conflict("import job has an invalid persisted binding");
    let source_id: Uuid = row.get("source_id");
    let version_id: Option<Uuid> = row.get("source_version_id");
    let input_hash: String = row.get("input_hash");
    if let Some(receipt) = receipt {
        let accepted_job = receipt.import_job.as_ref().ok_or_else(broken)?;
        let accepted_source = receipt.source.as_ref().ok_or_else(broken)?;
        if accepted_job.operation_id != row.get::<Uuid, _>("operation_id")
            || accepted_job.input_hash != input_hash
            || accepted_job.source_version_id != version_id
            || accepted_source.source_id != source_id
            || accepted_source.operator_id != scope.operator_id
            || accepted_source.tenant_id != scope.tenant_id
            || accepted_source.project_id != project
            || Some(accepted_source.purpose) != expected.map(|i| i.purpose)
        {
            return Err(broken());
        }
        if let Some(accepted_version) = receipt.source_version.as_ref()
            && (Some(accepted_version.source_version_id) != version_id
                || accepted_version.source_id != source_id
                || accepted_version.operator_id != scope.operator_id
                || accepted_version.tenant_id != scope.tenant_id
                || accepted_version.project_id != project
                || accepted_version.content_sha256 != input_hash
                || row.get::<Option<Uuid>, _>("version_object_id") != accepted_version.object_id
                || row.get::<Option<i64>, _>("version_object_version")
                    != accepted_version.object_version)
        {
            return Err(broken());
        }
    }
    let status = match row.get::<String, _>("status").as_str() {
        "queued" => ImportStatus::Queued,
        "running" => ImportStatus::Running,
        "partial" => ImportStatus::Partial,
        "succeeded" => ImportStatus::Succeeded,
        "failed" => ImportStatus::Failed,
        "cancelled" => ImportStatus::Cancelled,
        _ => return Err(broken()),
    };
    let stage = match row.get::<String, _>("stage").as_str() {
        "acquire" => ImportStage::Acquire,
        "parse" => ImportStage::Parse,
        "extract" => ImportStage::Extract,
        "index" => ImportStage::Index,
        "release" => ImportStage::Release,
        _ => return Err(broken()),
    };
    if row.get::<Option<Uuid>, _>("actual_operation_id") != Some(row.get("operation_id"))
        || row.get::<String, _>("operation_kind") != "knowledge.import"
        || row.get::<Option<Uuid>, _>("actual_version_id") != version_id
        || (version_id.is_some()
            && row.get::<Option<Uuid>, _>("version_source_id") != Some(source_id))
    {
        return Err(broken());
    }
    if let Some(expected) = expected
        && (row.get::<String, _>("source_kind") != super::source_kind(expected.kind)
            || row.get::<String, _>("source_name") != expected.name.trim()
            || (expected.object_id.is_some()
                && row.get::<Option<Uuid>, _>("version_object_id") != expected.object_id))
    {
        return Err(broken());
    }
    if let Some(version_id) = version_id {
        let hash: String = row.get("version_hash");
        if hash != input_hash {
            return Err(broken());
        }
        let object_id: Option<Uuid> = row.get("version_object_id");
        if let Some(object_id) = object_id
            && (row.get::<Option<Uuid>, _>("actual_object_id") != Some(object_id)
                || row.get::<Option<i64>, _>("actual_object_version")
                    != row.get::<Option<i64>, _>("version_object_version")
                || row.get::<Option<String>, _>("object_hash").as_deref() != Some(hash.as_str())
                || row.get::<Option<String>, _>("object_state").as_deref() != Some("committed")
                || row
                    .get::<Value, _>("locator")
                    .get("object_id")
                    .and_then(Value::as_str)
                    != Some(object_id.to_string().as_str()))
        {
            return Err(broken());
        }
        let is_pdf =
            row.get::<Option<String>, _>("object_media_type").as_deref() == Some("application/pdf");
        let task_id: Option<Uuid> = row.get("task_id");
        if is_pdf != task_id.is_some() {
            return Err(broken());
        }
        if let Some(task_id) = task_id
            && (task_id != job_id
                || row.get::<Option<Uuid>, _>("task_source_id") != Some(source_id)
                || row.get::<Option<Uuid>, _>("task_version_id") != Some(version_id)
                || row.get::<Option<Uuid>, _>("task_object_id") != object_id
                || row.get::<Option<i64>, _>("task_object_version")
                    != row.get::<Option<i64>, _>("version_object_version")
                || row.get::<Option<String>, _>("task_hash").as_deref() != Some(hash.as_str()))
        {
            return Err(broken());
        }
    } else if row.get::<Option<Uuid>, _>("task_id").is_some() {
        return Err(broken());
    }
    let ready = matches!(status, ImportStatus::Succeeded | ImportStatus::Partial);
    let mut release_id = None;
    if ready {
        let operation_status: String = row.get("operation_status");
        if operation_status != "succeeded" || version_id.is_none() || stage != ImportStage::Release
        {
            return Err(broken());
        }
        let result: Option<Value> = row.get("operation_result");
        let result = result.ok_or_else(broken)?;
        if result.get("source_id").and_then(Value::as_str) != Some(source_id.to_string().as_str())
            || result.get("source_version_id").and_then(Value::as_str)
                != version_id.map(|v| v.to_string()).as_deref()
        {
            return Err(broken());
        }
        release_id = if row.get::<Option<Uuid>, _>("task_id").is_some() {
            let task_release: Option<Uuid> = row.get("released_id");
            if result.get("knowledge_release_id").and_then(Value::as_str)
                != task_release.map(|v| v.to_string()).as_deref()
            {
                return Err(broken());
            }
            task_release
        } else {
            result
                .get("knowledge_release_id")
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok())
        };
        let release = release_id.ok_or_else(broken)?;
        let included: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM knowledge_release_source_versions membership
                JOIN knowledge_releases release
                  ON release.knowledge_release_id=membership.knowledge_release_id
                 AND release.operator_id=membership.operator_id
                 AND release.tenant_id=membership.tenant_id
                 AND release.project_id=membership.project_id
                WHERE membership.operator_id=$1 AND membership.tenant_id=$2
                  AND membership.project_id=$3 AND membership.knowledge_release_id=$4
                  AND membership.source_version_id=$5)",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid())
        .bind(release)
        .bind(version_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(database_error)?;
        if !included {
            return Err(broken());
        }
    } else if row.get::<Option<Uuid>, _>("released_id").is_some() {
        return Err(broken());
    }
    if let Some(receipt) = receipt
        && let Some(original_release) = receipt.release.as_ref()
        && (receipt.status != ImportStatus::Succeeded
            || release_id != Some(original_release.knowledge_release_id))
    {
        return Err(broken());
    }
    let raw_errors: Value = row.get("errors");
    let raw_errors = raw_errors.as_array().ok_or_else(broken)?;
    let (error_count, errors) = knowledge_import_progress_errors(raw_errors);
    Ok(Some(KnowledgeImportProgress {
        import_job_id: Some(job_id),
        status,
        stage: Some(stage),
        source_id: Some(source_id),
        source_version_id: if ready { version_id } else { None },
        knowledge_release_id: release_id,
        completed_units: row.get("completed_units"),
        failed_units: row.get("failed_units"),
        error_count,
        errors,
    }))
}

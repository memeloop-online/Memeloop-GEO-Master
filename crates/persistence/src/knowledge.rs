//! PostgreSQL implementation of the W03 knowledge repository.
//!
//! It deliberately supports only the same deterministic text pipeline as the
//! in-process implementation.  Unavailable adapters fail with an explicit
//! capability error; they never create queued work that appears to progress.
//! This implementation does not manufacture chunks,
//! facts, a vector index, or an LLM answer.

use async_trait::async_trait;
use chrono::{Duration, Utc};
use geo_domain::{
    AppError, Chunk, CurrentKnowledgeRelease, Fact, ImportAcceptance, ImportBatchAcceptance,
    ImportItem, ImportJob, ImportStage, ImportStatus, KnowledgeAnswerStatus, KnowledgeAskResult,
    KnowledgeCapability, KnowledgeCoverage, KnowledgeEvidence, KnowledgeOverview, KnowledgePurpose,
    KnowledgeRelease, KnowledgeRepository, KnowledgeSearchRequest, KnowledgeSearchResult,
    MAX_INLINE_TEXT_BYTES, MAX_UPLOAD_BYTES, Operation, OperationStatus, Product, Source,
    SourceDetail, SourceKind, SourceState, SourceVersion, StoredObject, StoredObjectState,
    TenantScope, UPLOAD_SESSION_TTL_SECONDS, UploadSession, UploadSessionCommand,
    UploadSessionState, deterministic_chunks, sha256_hex,
};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{Database, set_local_scope};

#[derive(Clone)]
pub struct PgKnowledgeRepository {
    pool: PgPool,
}

impl PgKnowledgeRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn from_database(database: &Database) -> Self {
        Self::new(database.pool().clone())
    }

    fn project_id(scope: &TenantScope) -> Result<geo_domain::ProjectId, AppError> {
        scope.project_id.ok_or_else(|| {
            AppError::invalid_request("project_id is required for knowledge resources")
        })
    }

    async fn transaction(
        &self,
        scope: &TenantScope,
    ) -> Result<Transaction<'_, Postgres>, AppError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        set_local_scope(&mut transaction, scope)
            .await
            .map_err(database_error)?;
        Ok(transaction)
    }

    async fn current_release_in_transaction(
        transaction: &mut Transaction<'_, Postgres>,
        scope: &TenantScope,
    ) -> Result<CurrentKnowledgeRelease, AppError> {
        let project_id = Self::project_id(scope)?;
        let row = sqlx::query(
            "SELECT release.knowledge_release_id, release.sequence
             FROM knowledge_current_releases pointer
             JOIN knowledge_releases release
               ON release.knowledge_release_id = pointer.knowledge_release_id
             WHERE pointer.operator_id=$1 AND pointer.tenant_id=$2 AND pointer.project_id=$3",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)?;
        Ok(CurrentKnowledgeRelease {
            project_id,
            knowledge_release_id: row.as_ref().map(|value| value.get("knowledge_release_id")),
            sequence: row.as_ref().map(|value| value.get("sequence")),
        })
    }

    async fn create_release(
        transaction: &mut Transaction<'_, Postgres>,
        scope: &TenantScope,
    ) -> Result<KnowledgeRelease, AppError> {
        let project_id = Self::project_id(scope)?;
        // Serialize sequence/current-pointer updates for this project.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(scope.storage_key())
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
        let previous = Self::current_release_in_transaction(transaction, scope).await?;
        let sequence = previous.sequence.unwrap_or(0) + 1;
        let versions = sqlx::query_scalar::<_, Uuid>(
            "SELECT version.source_version_id
             FROM knowledge_source_versions version
             JOIN knowledge_sources source ON source.source_id = version.source_id
             WHERE version.operator_id=$1 AND version.tenant_id=$2 AND version.project_id=$3
               AND source.state='active'
               AND EXISTS (SELECT 1 FROM knowledge_chunks chunk WHERE chunk.source_version_id=version.source_version_id)
             ORDER BY version.source_version_id",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .fetch_all(&mut **transaction)
        .await
        .map_err(database_error)?;
        let fact_refs = sqlx::query_as::<_, (Uuid, i64)>(
            "SELECT fact_id, revision FROM knowledge_facts
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 ORDER BY fact_id",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .fetch_all(&mut **transaction)
        .await
        .map_err(database_error)?;
        let chunk_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM knowledge_chunks
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3
               AND source_version_id = ANY($4)",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(&versions)
        .fetch_one(&mut **transaction)
        .await
        .map_err(database_error)?;
        let failed_source_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM knowledge_import_jobs
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND status IN ('failed','partial')",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .fetch_one(&mut **transaction)
        .await
        .map_err(database_error)?;
        let content_hash = sha256_hex(
            versions
                .iter()
                .map(Uuid::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                .as_bytes(),
        );
        let coverage = KnowledgeCoverage {
            source_version_count: versions.len() as u64,
            chunk_count: chunk_count as u64,
            failed_source_count: failed_source_count as u64,
            blocked_reasons: Vec::new(),
        };
        let release = KnowledgeRelease {
            knowledge_release_id: Uuid::new_v4(),
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id,
            sequence,
            previous_release_id: previous.knowledge_release_id,
            source_version_refs: versions.clone(),
            fact_revision_refs: fact_refs.clone(),
            index_build_id: "deterministic-text-index-v1".to_owned(),
            pipeline_versions: json!({"parser":"deterministic-text-v1","extractor":"none-v1","index":"substring-v1"}),
            content_hash,
            coverage,
            created_at: Utc::now(),
        };
        sqlx::query(
            "INSERT INTO knowledge_releases
             (knowledge_release_id,operator_id,tenant_id,project_id,sequence,previous_release_id,index_build_id,pipeline_versions,content_hash,coverage,created_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(release.knowledge_release_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(release.sequence)
        .bind(release.previous_release_id)
        .bind(&release.index_build_id)
        .bind(&release.pipeline_versions)
        .bind(&release.content_hash)
        .bind(serde_json::to_value(&release.coverage).map_err(serialization_error)?)
        .bind(release.created_at)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        for version_id in &versions {
            sqlx::query(
                "INSERT INTO knowledge_release_source_versions
                 (knowledge_release_id,operator_id,tenant_id,project_id,source_version_id)
                 VALUES ($1,$2,$3,$4,$5)",
            )
            .bind(release.knowledge_release_id)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project_id.as_uuid())
            .bind(version_id)
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
        }
        for (fact_id, revision) in &fact_refs {
            sqlx::query(
                "INSERT INTO knowledge_release_facts
                 (knowledge_release_id,operator_id,tenant_id,project_id,fact_id,fact_revision)
                 VALUES ($1,$2,$3,$4,$5,$6)",
            )
            .bind(release.knowledge_release_id)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project_id.as_uuid())
            .bind(fact_id)
            .bind(revision)
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
        }
        sqlx::query(
            "INSERT INTO knowledge_current_releases
             (operator_id,tenant_id,project_id,knowledge_release_id,updated_at)
             VALUES ($1,$2,$3,$4,now())
             ON CONFLICT (operator_id,tenant_id,project_id)
             DO UPDATE SET knowledge_release_id=excluded.knowledge_release_id, updated_at=now()",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(release.knowledge_release_id)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "INSERT INTO outbox_events
             (event_id,event_type,schema_version,operator_id,tenant_id,project_id,aggregate_id,aggregate_version,occurred_at,correlation_id,payload)
             VALUES ($1,'knowledge.release.created',1,$2,$3,$4,$5,$6,$7,$8,$9)",
        )
        .bind(Uuid::new_v4())
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(release.knowledge_release_id)
        .bind(release.sequence)
        .bind(release.created_at)
        .bind(release.knowledge_release_id)
        .bind(serde_json::to_value(&release).map_err(serialization_error)?)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        Ok(release)
    }

    async fn import_text_in_transaction(
        transaction: &mut Transaction<'_, Postgres>,
        scope: &TenantScope,
        item: &ImportItem,
        object: Option<StoredObject>,
        text: String,
    ) -> Result<ImportAcceptance, AppError> {
        if text.trim().is_empty() {
            return Err(AppError::invalid_request("text must not be empty"));
        }
        let project_id = Self::project_id(scope)?;
        let now = Utc::now();
        let source_id = Uuid::new_v4();
        let source_version_id = Uuid::new_v4();
        let operation_id = Uuid::new_v4();
        let job_id = Uuid::new_v4();
        let content_hash = sha256_hex(text.as_bytes());
        if let Some(object) = &object {
            sqlx::query(
                "INSERT INTO knowledge_stored_objects
                 (object_id,operator_id,tenant_id,project_id,object_version,backend,opaque_key,actual_size,detected_media_type,sha256,state,created_at)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,'committed',$11)",
            )
            .bind(object.object_id)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project_id.as_uuid())
            .bind(object.object_version)
            .bind(&object.backend)
            .bind(&object.opaque_key)
            .bind(object.actual_size as i64)
            .bind(&object.detected_media_type)
            .bind(&object.sha256)
            .bind(object.created_at)
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
        }
        sqlx::query(
            "INSERT INTO knowledge_sources
             (source_id,operator_id,tenant_id,project_id,revision,kind,name,purpose,state,locator,current_version_id,sync_enabled)
             VALUES ($1,$2,$3,$4,1,$5,$6,$7,'active',$8,NULL,false)",
        )
        .bind(source_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(source_kind(item.kind))
        .bind(item.name.trim())
        .bind(purpose(item.purpose))
        .bind(json!({"kind":"inline_text"}))
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "INSERT INTO knowledge_source_versions
             (source_version_id,operator_id,tenant_id,project_id,source_id,version,object_id,object_version,content_sha256,captured_at,parser_version,extraction_version,created_at)
             VALUES ($1,$2,$3,$4,$5,1,$6,$7,$8,$9,'deterministic-text-v1','none-v1',$9)",
        )
        .bind(source_version_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(source_id)
        .bind(object.as_ref().map(|value| value.object_id))
        .bind(object.as_ref().map(|value| value.object_version))
        .bind(&content_hash)
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "UPDATE knowledge_sources SET current_version_id=$1,updated_at=now()
             WHERE source_id=$2 AND operator_id=$3 AND tenant_id=$4 AND project_id=$5",
        )
        .bind(source_version_id)
        .bind(source_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        let chunks = deterministic_chunks(scope, source_version_id, &text);
        for chunk in &chunks {
            sqlx::query(
                "INSERT INTO knowledge_chunks
                 (chunk_id,operator_id,tenant_id,project_id,source_version_id,ordinal,kind,text,text_hash,locator,product_ids,market,language,extraction_method,confidence)
                 VALUES ($1,$2,$3,$4,$5,$6,'paragraph',$7,$8,$9,$10,$11,$12,$13,$14)",
            )
            .bind(chunk.chunk_id)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project_id.as_uuid())
            .bind(source_version_id)
            .bind(chunk.ordinal)
            .bind(&chunk.text)
            .bind(&chunk.text_hash)
            .bind(serde_json::to_value(&chunk.locator).map_err(serialization_error)?)
            .bind(json!([]))
            .bind(&chunk.market)
            .bind(&chunk.language)
            .bind(&chunk.extraction_method)
            .bind(chunk.confidence)
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
        }
        let source = Source {
            source_id,
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id,
            revision: 1,
            kind: item.kind,
            name: item.name.trim().to_owned(),
            purpose: item.purpose,
            state: SourceState::Active,
            locator: json!({"kind":"inline_text"}),
            current_version_id: Some(source_version_id),
            sync_enabled: false,
            next_sync_at: None,
            last_sync_at: None,
        };
        let version = SourceVersion {
            source_version_id,
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id,
            source_id,
            version: 1,
            object_id: object.as_ref().map(|value| value.object_id),
            object_version: object.as_ref().map(|value| value.object_version),
            content_sha256: content_hash.clone(),
            captured_at: now,
            original_url: None,
            parent_version_id: None,
            parser_version: "deterministic-text-v1".to_owned(),
            extraction_version: "none-v1".to_owned(),
            created_at: now,
        };
        let operation = Operation {
            id: operation_id,
            kind: "knowledge.import".to_owned(),
            status: OperationStatus::Succeeded,
            scope: scope.clone(),
            result: None,
            error: None,
            created_at: now,
            updated_at: now,
        };
        let job = ImportJob {
            import_job_id: job_id,
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id,
            operation_id,
            source_id,
            source_version_id: Some(source_version_id),
            stage: ImportStage::Release,
            status: ImportStatus::Succeeded,
            attempt: 1,
            lease_until: None,
            input_hash: content_hash,
            stage_output_refs: vec![format!("chunks:{}", chunks.len())],
            completed_units: chunks.len() as i32,
            failed_units: 0,
            errors: Vec::new(),
            resumed_from: None,
        };
        // Operation, job, release, and outbox are inserted before the same
        // transaction commits.  This is the durable acceptance boundary.
        sqlx::query(
            "INSERT INTO operations (operation_id,operator_id,tenant_id,project_id,kind,status,result,error,created_at,updated_at)
             VALUES ($1,$2,$3,$4,'knowledge.import','succeeded',NULL,NULL,$5,$5)",
        )
        .bind(operation_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "INSERT INTO knowledge_import_jobs
             (import_job_id,operator_id,tenant_id,project_id,operation_id,source_id,source_version_id,stage,status,attempt,input_hash,stage_output_refs,completed_units,failed_units,errors)
             VALUES ($1,$2,$3,$4,$5,$6,$7,'release','succeeded',1,$8,$9,$10,0,'[]'::jsonb)",
        )
        .bind(job_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(operation_id)
        .bind(source_id)
        .bind(source_version_id)
        .bind(&job.input_hash)
        .bind(serde_json::to_value(&job.stage_output_refs).map_err(serialization_error)?)
        .bind(job.completed_units)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        let release = Self::create_release(transaction, scope).await?;
        let mut operation = operation;
        operation.result = Some(json!({
            "source_id": source_id,
            "source_version_id": source_version_id,
            "knowledge_release_id": release.knowledge_release_id
        }));
        sqlx::query("UPDATE operations SET result=$1 WHERE operation_id=$2")
            .bind(operation.result.clone())
            .bind(operation_id)
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
        Ok(ImportAcceptance {
            client_item_id: item.client_item_id.clone(),
            status: ImportStatus::Succeeded,
            source: Some(source),
            source_version: Some(version),
            import_job: Some(job),
            operation: Some(operation),
            release: Some(release),
            error: None,
        })
    }

    async fn failed_adapter_in_transaction(
        transaction: &mut Transaction<'_, Postgres>,
        scope: &TenantScope,
        item: &ImportItem,
        object: StoredObject,
        capability: &str,
    ) -> Result<ImportAcceptance, AppError> {
        let project_id = Self::project_id(scope)?;
        let now = Utc::now();
        let source_id = Uuid::new_v4();
        let operation_id = Uuid::new_v4();
        let job_id = Uuid::new_v4();
        let error = AppError::capability_missing(format!("{capability} is not configured"));
        sqlx::query(
            "INSERT INTO knowledge_stored_objects
             (object_id,operator_id,tenant_id,project_id,object_version,backend,opaque_key,actual_size,detected_media_type,sha256,state,created_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,'committed',$11)",
        )
        .bind(object.object_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(object.object_version)
        .bind(&object.backend)
        .bind(&object.opaque_key)
        .bind(object.actual_size as i64)
        .bind(&object.detected_media_type)
        .bind(&object.sha256)
        .bind(object.created_at)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "INSERT INTO knowledge_sources
             (source_id,operator_id,tenant_id,project_id,revision,kind,name,purpose,state,locator,current_version_id,sync_enabled)
             VALUES ($1,$2,$3,$4,1,$5,$6,$7,'active',$8,NULL,false)",
        )
        .bind(source_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(source_kind(item.kind))
        .bind(item.name.trim())
        .bind(purpose(item.purpose))
        .bind(json!({"kind":"blocked","capability":capability}))
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        let operation = Operation {
            id: operation_id,
            kind: "knowledge.import".to_owned(),
            status: OperationStatus::Failed,
            scope: scope.clone(),
            result: None,
            error: Some(error.clone()),
            created_at: now,
            updated_at: now,
        };
        sqlx::query(
            "INSERT INTO operations (operation_id,operator_id,tenant_id,project_id,kind,status,result,error,created_at,updated_at)
             VALUES ($1,$2,$3,$4,'knowledge.import','failed',NULL,$5,$6,$6)",
        )
        .bind(operation_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(serde_json::to_value(&error).map_err(serialization_error)?)
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        let job = ImportJob {
            import_job_id: job_id,
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id,
            operation_id,
            source_id,
            source_version_id: None,
            stage: ImportStage::Parse,
            status: ImportStatus::Failed,
            attempt: 1,
            lease_until: None,
            input_hash: object.sha256.clone(),
            stage_output_refs: Vec::new(),
            completed_units: 0,
            failed_units: 1,
            errors: vec![json!({"code":"capability_missing","capability":capability})],
            resumed_from: None,
        };
        sqlx::query(
            "INSERT INTO knowledge_import_jobs
             (import_job_id,operator_id,tenant_id,project_id,operation_id,source_id,source_version_id,stage,status,attempt,input_hash,stage_output_refs,completed_units,failed_units,errors)
             VALUES ($1,$2,$3,$4,$5,$6,NULL,'parse','failed',1,$7,'[]'::jsonb,0,1,$8)",
        )
        .bind(job_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(operation_id)
        .bind(source_id)
        .bind(&job.input_hash)
        .bind(serde_json::to_value(&job.errors).map_err(serialization_error)?)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "INSERT INTO outbox_events
             (event_id,event_type,schema_version,operator_id,tenant_id,project_id,aggregate_id,aggregate_version,occurred_at,correlation_id,payload)
             VALUES ($1,'knowledge.import.failed',1,$2,$3,$4,$5,1,$6,$7,$8)",
        )
        .bind(Uuid::new_v4())
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(source_id)
        .bind(now)
        .bind(operation_id)
        .bind(json!({"capability":capability,"source_id":source_id}))
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        Ok(ImportAcceptance {
            client_item_id: item.client_item_id.clone(),
            status: ImportStatus::Failed,
            source: Some(Source {
                source_id,
                operator_id: scope.operator_id,
                tenant_id: scope.tenant_id,
                project_id,
                revision: 1,
                kind: item.kind,
                name: item.name.trim().to_owned(),
                purpose: item.purpose,
                state: SourceState::Active,
                locator: json!({"kind":"blocked","capability":capability}),
                current_version_id: None,
                sync_enabled: false,
                next_sync_at: None,
                last_sync_at: None,
            }),
            source_version: None,
            import_job: Some(job),
            operation: Some(operation),
            release: None,
            error: Some(error),
        })
    }
}

#[async_trait]
impl KnowledgeRepository for PgKnowledgeRepository {
    async fn capabilities(&self, scope: &TenantScope) -> Result<KnowledgeCapability, AppError> {
        Self::project_id(scope)?;
        Ok(KnowledgeCapability::durable_text_only())
    }

    async fn create_upload_session(
        &self,
        scope: &TenantScope,
        command: UploadSessionCommand,
    ) -> Result<UploadSession, AppError> {
        let project_id = Self::project_id(scope)?;
        if command.filename.trim().is_empty() || command.filename.len() > 255 {
            return Err(AppError::invalid_request(
                "filename must be between 1 and 255 characters",
            ));
        }
        if command.expected_size == 0
            || command.expected_size > MAX_UPLOAD_BYTES
            || command.expected_sha256.len() != 64
            || !command
                .expected_sha256
                .bytes()
                .all(|value| value.is_ascii_hexdigit())
        {
            return Err(AppError::invalid_request(
                "invalid upload size or expected_sha256",
            ));
        }
        let now = Utc::now();
        let session = UploadSession {
            upload_session_id: Uuid::new_v4(),
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id,
            revision: 1,
            filename: command.filename.trim().to_owned(),
            declared_media_type: command
                .declared_media_type
                .split(';')
                .next()
                .unwrap_or(&command.declared_media_type)
                .trim()
                .to_ascii_lowercase(),
            expected_size: command.expected_size,
            expected_sha256: command.expected_sha256.to_ascii_lowercase(),
            purpose: command.purpose,
            state: UploadSessionState::Created,
            expires_at: now + Duration::seconds(UPLOAD_SESSION_TTL_SECONDS),
            staging_object_ref: Some(format!("postgres-staging/{}", Uuid::new_v4())),
            committed_object_id: None,
            operation_id: None,
        };
        let mut transaction = self.transaction(scope).await?;
        sqlx::query(
            "INSERT INTO knowledge_upload_sessions
             (upload_session_id,operator_id,tenant_id,project_id,revision,filename,declared_media_type,expected_size,expected_sha256,purpose,state,expires_at,staging_object_ref,created_at,updated_at)
             VALUES ($1,$2,$3,$4,1,$5,$6,$7,$8,$9,'created',$10,$11,$12,$12)",
        )
        .bind(session.upload_session_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(&session.filename)
        .bind(&session.declared_media_type)
        .bind(session.expected_size as i64)
        .bind(&session.expected_sha256)
        .bind(purpose(session.purpose))
        .bind(session.expires_at)
        .bind(&session.staging_object_ref)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(session)
    }

    async fn put_upload_content(
        &self,
        scope: &TenantScope,
        id: Uuid,
        content: Vec<u8>,
    ) -> Result<UploadSession, AppError> {
        let project_id = Self::project_id(scope)?;
        let mut transaction = self.transaction(scope).await?;
        let row = sqlx::query(
            "SELECT revision,filename,declared_media_type,expected_size,expected_sha256,purpose,state,expires_at,staging_object_ref,committed_object_id,operation_id
             FROM knowledge_upload_sessions WHERE upload_session_id=$1 AND operator_id=$2 AND tenant_id=$3 AND project_id=$4 FOR UPDATE",
        )
        .bind(id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project_id.as_uuid())
        .fetch_optional(&mut *transaction).await.map_err(database_error)?
        .ok_or_else(|| AppError::not_found("upload session not found"))?;
        let expires_at = row.get("expires_at");
        if expires_at <= Utc::now() {
            sqlx::query("UPDATE knowledge_upload_sessions SET state='expired',revision=revision+1 WHERE upload_session_id=$1")
                .bind(id).execute(&mut *transaction).await.map_err(database_error)?;
            transaction.commit().await.map_err(database_error)?;
            return Err(AppError::conflict("upload session has expired"));
        }
        let state: String = row.get("state");
        if !matches!(state.as_str(), "created" | "uploaded") {
            transaction.commit().await.map_err(database_error)?;
            return Err(AppError::conflict("upload session cannot accept content"));
        }
        let expected_size: i64 = row.get("expected_size");
        if content.len() as i64 > expected_size || content.len() as u64 > MAX_UPLOAD_BYTES {
            return Err(AppError::invalid_request(
                "uploaded content exceeds declared size",
            ));
        }
        let actual_hash = sha256_hex(&content);
        sqlx::query(
            "INSERT INTO knowledge_upload_blobs (upload_session_id,content,actual_size,sha256)
             VALUES ($1,$2,$3,$4)
             ON CONFLICT (upload_session_id) DO UPDATE SET content=excluded.content,actual_size=excluded.actual_size,sha256=excluded.sha256,created_at=now()",
        ).bind(id).bind(&content).bind(content.len() as i64).bind(actual_hash)
        .execute(&mut *transaction).await.map_err(database_error)?;
        sqlx::query("UPDATE knowledge_upload_sessions SET state='uploaded',revision=revision+1,updated_at=now() WHERE upload_session_id=$1")
            .bind(id).execute(&mut *transaction).await.map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(UploadSession {
            upload_session_id: id,
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id,
            revision: row.get::<i64, _>("revision") + 1,
            filename: row.get("filename"),
            declared_media_type: row.get("declared_media_type"),
            expected_size: expected_size as u64,
            expected_sha256: row.get("expected_sha256"),
            purpose: parse_purpose(&row.get::<String, _>("purpose"))?,
            state: UploadSessionState::Uploaded,
            expires_at,
            staging_object_ref: row.get("staging_object_ref"),
            committed_object_id: row.get("committed_object_id"),
            operation_id: row.get("operation_id"),
        })
    }

    async fn complete_upload(
        &self,
        scope: &TenantScope,
        id: Uuid,
        idempotency_key: &str,
    ) -> Result<ImportAcceptance, AppError> {
        if idempotency_key.trim().is_empty() {
            return Err(AppError::invalid_request(
                "Idempotency-Key must not be empty",
            ));
        }
        let project_id = Self::project_id(scope)?;
        let request_hash = sha256_hex(idempotency_key.trim().as_bytes());
        let mut transaction = self.transaction(scope).await?;
        if let Some(receipt) = sqlx::query(
            "SELECT request_hash,acceptance FROM knowledge_import_receipts
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3
               AND action='upload_complete' AND target_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        {
            let stored_hash: String = receipt.get("request_hash");
            let acceptance: Value = receipt.get("acceptance");
            transaction.commit().await.map_err(database_error)?;
            if stored_hash != request_hash {
                return Err(AppError::conflict(
                    "upload session was completed with a different idempotency key",
                ));
            }
            return serde_json::from_value(acceptance).map_err(serialization_error);
        }
        let row = sqlx::query(
            "SELECT session.filename,session.declared_media_type,session.expected_size,session.expected_sha256,session.purpose,session.state,session.expires_at,
                    blob.content,blob.actual_size,blob.sha256
             FROM knowledge_upload_sessions session LEFT JOIN knowledge_upload_blobs blob ON blob.upload_session_id=session.upload_session_id
             WHERE session.upload_session_id=$1 AND session.operator_id=$2 AND session.tenant_id=$3 AND session.project_id=$4 FOR UPDATE",
        ).bind(id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project_id.as_uuid())
        .fetch_optional(&mut *transaction).await.map_err(database_error)?
        .ok_or_else(|| AppError::not_found("upload session not found"))?;
        let state: String = row.get("state");
        if row.get::<chrono::DateTime<Utc>, _>("expires_at") <= Utc::now() {
            sqlx::query("UPDATE knowledge_upload_sessions SET state='expired',revision=revision+1,updated_at=now() WHERE upload_session_id=$1")
                .bind(id).execute(&mut *transaction).await.map_err(database_error)?;
            transaction.commit().await.map_err(database_error)?;
            return Err(AppError::conflict("upload session has expired"));
        }
        if state == "committed" {
            transaction.commit().await.map_err(database_error)?;
            return Err(AppError::conflict("upload session was already completed"));
        }
        if state != "uploaded" {
            transaction.commit().await.map_err(database_error)?;
            return Err(AppError::conflict(
                "upload session content is not ready to complete",
            ));
        }
        let content: Option<Vec<u8>> = row.get("content");
        let actual_size: Option<i64> = row.get("actual_size");
        let actual_hash: Option<String> = row.get("sha256");
        let expected_size: i64 = row.get("expected_size");
        let expected_hash: String = row.get("expected_sha256");
        if content.is_none()
            || actual_size != Some(expected_size)
            || actual_hash.as_deref() != Some(expected_hash.as_str())
        {
            sqlx::query("UPDATE knowledge_upload_sessions SET state='failed',revision=revision+1,updated_at=now() WHERE upload_session_id=$1")
                .bind(id).execute(&mut *transaction).await.map_err(database_error)?;
            transaction.commit().await.map_err(database_error)?;
            return Err(AppError::invalid_request(
                "uploaded content size or sha256 does not match upload session",
            ));
        }
        let filename: String = row.get("filename");
        let media_type: String = row.get("declared_media_type");
        let purpose = parse_purpose(&row.get::<String, _>("purpose"))?;
        // The verified session marker, object, source/version, chunks, job,
        // operation, release, outbox event, and idempotency receipt share
        // this one transaction.  A failure rolls all of them back.
        let object_id = Uuid::new_v4();
        let item = ImportItem {
            client_item_id: format!("upload:{id}"),
            kind: SourceKind::File,
            name: filename,
            purpose,
            text: None,
            url: None,
            object_id: Some(object_id),
            knowledge_release_id: None,
        };
        let object = StoredObject {
            object_id,
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id,
            object_version: 1,
            backend: "postgres_blob".to_owned(),
            opaque_key: format!("upload/{id}"),
            actual_size: expected_size as u64,
            detected_media_type: media_type,
            sha256: expected_hash,
            state: StoredObjectState::Committed,
            created_at: Utc::now(),
        };
        let acceptance = if !is_text(&object.detected_media_type) {
            Self::failed_adapter_in_transaction(
                &mut transaction,
                scope,
                &item,
                object,
                "document_parser",
            )
            .await?
        } else {
            let text = match String::from_utf8(content.expect("validated content")) {
                Ok(text) => text,
                Err(_) => {
                    sqlx::query("UPDATE knowledge_upload_sessions SET state='failed',revision=revision+1,updated_at=now() WHERE upload_session_id=$1")
                        .bind(id).execute(&mut *transaction).await.map_err(database_error)?;
                    transaction.commit().await.map_err(database_error)?;
                    return Err(AppError::invalid_request(
                        "text upload bytes must be valid UTF-8",
                    ));
                }
            };
            Self::import_text_in_transaction(&mut transaction, scope, &item, Some(object), text)
                .await?
        };
        sqlx::query(
            "UPDATE knowledge_upload_sessions
             SET state='committed',revision=revision+1,committed_object_id=$1,
                 operation_id=$2,completion_idempotency_key_hash=$3,updated_at=now()
             WHERE upload_session_id=$4 AND state='uploaded'",
        )
        .bind(object_id)
        .bind(acceptance.operation.as_ref().map(|operation| operation.id))
        .bind(&request_hash)
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "INSERT INTO knowledge_import_receipts
             (knowledge_import_receipt_id,operator_id,tenant_id,project_id,action,target_id,client_item_id,request_hash,acceptance)
             VALUES ($1,$2,$3,$4,'upload_complete',$5,NULL,$6,$7)",
        )
        .bind(Uuid::new_v4())
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(id)
        .bind(&request_hash)
        .bind(serde_json::to_value(&acceptance).map_err(serialization_error)?)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(acceptance)
    }

    async fn import_batch(
        &self,
        scope: &TenantScope,
        items: Vec<ImportItem>,
    ) -> Result<ImportBatchAcceptance, AppError> {
        let project_id = Self::project_id(scope)?;
        if items.is_empty() || items.len() > 100 {
            return Err(AppError::invalid_request(
                "imports must contain between 1 and 100 items",
            ));
        }
        let mut output = Vec::with_capacity(items.len());
        for item in items {
            let input_hash = sha256_hex(&serde_json::to_vec(&item).map_err(serialization_error)?);
            let mut transaction = self.transaction(scope).await?;
            if let Some(receipt) = sqlx::query(
                "SELECT request_hash,acceptance FROM knowledge_import_receipts
                 WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3
                   AND action='import_item' AND client_item_id=$4",
            )
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project_id.as_uuid())
            .bind(&item.client_item_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(database_error)?
            {
                let stored_hash: String = receipt.get("request_hash");
                let acceptance: Value = receipt.get("acceptance");
                transaction.commit().await.map_err(database_error)?;
                if stored_hash != input_hash {
                    output.push(ImportAcceptance {
                        client_item_id: item.client_item_id,
                        status: ImportStatus::Failed,
                        source: None,
                        source_version: None,
                        import_job: None,
                        operation: None,
                        release: None,
                        error: Some(AppError::conflict(
                            "client_item_id was already used with different input",
                        )),
                    });
                } else {
                    output.push(serde_json::from_value(acceptance).map_err(serialization_error)?);
                }
                continue;
            }
            let acceptance = if item.kind == SourceKind::Text {
                match item.text.clone() {
                    Some(text) if text.len() <= MAX_INLINE_TEXT_BYTES => {
                        Self::import_text_in_transaction(&mut transaction, scope, &item, None, text)
                            .await
                    }
                    Some(_) => Err(AppError::invalid_request(
                        "text exceeds inline limit; use an upload session",
                    )),
                    None => Err(AppError::invalid_request(
                        "text imports require the text field",
                    )),
                }
            } else if item.kind == SourceKind::Url {
                Err(AppError::capability_missing("url_fetch is not configured"))
            } else {
                Err(AppError::capability_missing(
                    "import adapter is not configured",
                ))
            };
            let acceptance = acceptance.unwrap_or_else(|error| ImportAcceptance {
                client_item_id: item.client_item_id,
                status: ImportStatus::Failed,
                source: None,
                source_version: None,
                import_job: None,
                operation: None,
                release: None,
                error: Some(error),
            });
            sqlx::query(
                "INSERT INTO knowledge_import_receipts
                 (knowledge_import_receipt_id,operator_id,tenant_id,project_id,action,target_id,client_item_id,request_hash,acceptance)
                 VALUES ($1,$2,$3,$4,'import_item',$5,$6,$7,$8)",
            )
            .bind(Uuid::new_v4())
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project_id.as_uuid())
            .bind(Uuid::new_v4())
            .bind(&acceptance.client_item_id)
            .bind(input_hash)
            .bind(serde_json::to_value(&acceptance).map_err(serialization_error)?)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
            transaction.commit().await.map_err(database_error)?;
            output.push(acceptance);
        }
        Ok(ImportBatchAcceptance { items: output })
    }

    async fn list_sources(&self, scope: &TenantScope) -> Result<Vec<Source>, AppError> {
        let project_id = Self::project_id(scope)?;
        let mut transaction = self.transaction(scope).await?;
        let rows=sqlx::query("SELECT source_id,revision,kind,name,purpose,state,locator,current_version_id,sync_enabled,next_sync_at,last_sync_at FROM knowledge_sources WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 ORDER BY created_at DESC")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project_id.as_uuid()).fetch_all(&mut *transaction).await.map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        rows.into_iter()
            .map(|row| source_from_row(&row, scope, project_id))
            .collect()
    }

    async fn get_source(&self, scope: &TenantScope, id: Uuid) -> Result<Option<Source>, AppError> {
        let project_id = Self::project_id(scope)?;
        let mut transaction = self.transaction(scope).await?;
        let row=sqlx::query("SELECT source_id,revision,kind,name,purpose,state,locator,current_version_id,sync_enabled,next_sync_at,last_sync_at FROM knowledge_sources WHERE source_id=$1 AND operator_id=$2 AND tenant_id=$3 AND project_id=$4")
            .bind(id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project_id.as_uuid()).fetch_optional(&mut *transaction).await.map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        row.map(|row| source_from_row(&row, scope, project_id))
            .transpose()
    }

    async fn get_source_detail(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<SourceDetail>, AppError> {
        let Some(source) = self.get_source(scope, id).await? else {
            return Ok(None);
        };
        let project_id = Self::project_id(scope)?;
        let mut transaction = self.transaction(scope).await?;
        let version_rows=sqlx::query("SELECT source_version_id,source_id,version,object_id,object_version,content_sha256,captured_at,original_url,parent_version_id,parser_version,extraction_version,created_at FROM knowledge_source_versions WHERE source_id=$1 AND operator_id=$2 AND tenant_id=$3 AND project_id=$4 ORDER BY version")
            .bind(id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project_id.as_uuid()).fetch_all(&mut *transaction).await.map_err(database_error)?;
        let versions = version_rows
            .iter()
            .map(|row| version_from_row(row, scope, project_id))
            .collect::<Result<Vec<_>, _>>()?;
        let chunks_rows=sqlx::query("SELECT chunk_id,source_version_id,ordinal,kind,text,text_hash,locator,product_ids,market,language,extraction_method,confidence FROM knowledge_chunks WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND source_version_id IN (SELECT source_version_id FROM knowledge_source_versions WHERE source_id=$4) ORDER BY source_version_id,ordinal")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project_id.as_uuid()).bind(id).fetch_all(&mut *transaction).await.map_err(database_error)?;
        let chunks = chunks_rows
            .iter()
            .map(|row| chunk_from_row(row, scope, project_id))
            .collect::<Result<Vec<_>, _>>()?;
        let job_rows=sqlx::query("SELECT import_job_id,operation_id,source_id,source_version_id,stage,status,attempt,lease_until,input_hash,stage_output_refs,completed_units,failed_units,errors,resumed_from FROM knowledge_import_jobs WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND source_id=$4 ORDER BY created_at")
            .bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project_id.as_uuid()).bind(id).fetch_all(&mut *transaction).await.map_err(database_error)?;
        let jobs = job_rows
            .iter()
            .map(|row| job_from_row(row, scope, project_id))
            .collect::<Result<Vec<_>, _>>()?;
        transaction.commit().await.map_err(database_error)?;
        Ok(Some(SourceDetail {
            source,
            versions,
            chunks,
            facts: Vec::new(),
            import_jobs: jobs,
        }))
    }

    async fn get_source_version(
        &self,
        scope: &TenantScope,
        source_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<SourceVersion>, AppError> {
        let project_id = Self::project_id(scope)?;
        let mut transaction = self.transaction(scope).await?;
        let row=sqlx::query("SELECT source_version_id,source_id,version,object_id,object_version,content_sha256,captured_at,original_url,parent_version_id,parser_version,extraction_version,created_at FROM knowledge_source_versions WHERE source_version_id=$1 AND source_id=$2 AND operator_id=$3 AND tenant_id=$4 AND project_id=$5")
            .bind(version_id).bind(source_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project_id.as_uuid()).fetch_optional(&mut *transaction).await.map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        row.map(|row| version_from_row(&row, scope, project_id))
            .transpose()
    }

    async fn list_products(&self, scope: &TenantScope) -> Result<Vec<Product>, AppError> {
        Self::project_id(scope)?;
        Ok(Vec::new())
    }
    async fn list_facts(&self, scope: &TenantScope) -> Result<Vec<Fact>, AppError> {
        Self::project_id(scope)?;
        Ok(Vec::new())
    }

    async fn current_release(
        &self,
        scope: &TenantScope,
    ) -> Result<CurrentKnowledgeRelease, AppError> {
        let mut transaction = self.transaction(scope).await?;
        let value = Self::current_release_in_transaction(&mut transaction, scope).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(value)
    }

    async fn get_release(
        &self,
        scope: &TenantScope,
        id: Uuid,
    ) -> Result<Option<KnowledgeRelease>, AppError> {
        let project_id = Self::project_id(scope)?;
        let mut transaction = self.transaction(scope).await?;
        let row=sqlx::query("SELECT knowledge_release_id,sequence,previous_release_id,index_build_id,pipeline_versions,content_hash,coverage,created_at FROM knowledge_releases WHERE knowledge_release_id=$1 AND operator_id=$2 AND tenant_id=$3 AND project_id=$4")
            .bind(id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project_id.as_uuid()).fetch_optional(&mut *transaction).await.map_err(database_error)?;
        let result = if let Some(row) = row {
            Some(release_from_row(&mut transaction, scope, project_id, &row).await?)
        } else {
            None
        };
        transaction.commit().await.map_err(database_error)?;
        Ok(result)
    }

    async fn search(
        &self,
        scope: &TenantScope,
        request: KnowledgeSearchRequest,
    ) -> Result<KnowledgeSearchResult, AppError> {
        let project_id = Self::project_id(scope)?;
        if request.query.trim().is_empty() {
            return Err(AppError::invalid_request("query must not be empty"));
        }
        let mut transaction = self.transaction(scope).await?;
        let current = Self::current_release_in_transaction(&mut transaction, scope).await?;
        let release_id = request
            .knowledge_release_id
            .or(current.knowledge_release_id);
        let Some(release_id) = release_id else {
            transaction.commit().await.map_err(database_error)?;
            return Ok(KnowledgeSearchResult {
                knowledge_release_id: None,
                evidence: Vec::new(),
                capability_missing: None,
            });
        };
        if request.knowledge_release_id.is_some() {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM knowledge_releases WHERE knowledge_release_id=$1 AND operator_id=$2 AND tenant_id=$3 AND project_id=$4)",
            )
            .bind(release_id)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project_id.as_uuid())
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)?;
            if !exists {
                transaction.commit().await.map_err(database_error)?;
                return Err(AppError::not_found("knowledge release not found"));
            }
        }
        let count = request.limit.clamp(1, 50) as i64;
        let rows=sqlx::query(
            "SELECT source.source_id AS evidence_source_id,source.name AS evidence_source_name,source.purpose AS evidence_purpose,
                    chunk.chunk_id,chunk.source_version_id,chunk.ordinal,chunk.kind,chunk.text,chunk.text_hash,chunk.locator,chunk.product_ids,chunk.market,chunk.language,chunk.extraction_method,chunk.confidence
             FROM knowledge_release_source_versions member JOIN knowledge_source_versions version ON version.source_version_id=member.source_version_id
             JOIN knowledge_sources source ON source.source_id=version.source_id JOIN knowledge_chunks chunk ON chunk.source_version_id=version.source_version_id
             WHERE member.knowledge_release_id=$1 AND member.operator_id=$2 AND member.tenant_id=$3 AND member.project_id=$4
               AND source.state='active' AND ($5='internal' OR source.purpose='public')
               AND strpos(lower(chunk.text), lower($6)) > 0
             ORDER BY version.source_version_id,chunk.ordinal LIMIT $7",
        ).bind(release_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project_id.as_uuid()).bind(purpose(request.purpose)).bind(request.query.trim()).bind(count)
         .fetch_all(&mut *transaction).await.map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(KnowledgeSearchResult {
            knowledge_release_id: Some(release_id),
            evidence: rows
                .into_iter()
                .map(|row| evidence_from_row(&row))
                .collect::<Result<_, _>>()?,
            capability_missing: None,
        })
    }

    async fn ask(
        &self,
        scope: &TenantScope,
        request: KnowledgeSearchRequest,
    ) -> Result<KnowledgeAskResult, AppError> {
        let search = self.search(scope, request).await?;
        let answer = if search.evidence.is_empty() {
            "当前资料没有这项信息".to_owned()
        } else {
            search
                .evidence
                .iter()
                .map(|value| value.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n")
        };
        let answer_status = if search.evidence.is_empty() {
            KnowledgeAnswerStatus::InsufficientEvidence
        } else {
            KnowledgeAnswerStatus::Answered
        };
        Ok(KnowledgeAskResult {
            knowledge_release_id: search.knowledge_release_id,
            mode: "evidence_only".to_owned(),
            answer_status,
            answer,
            evidence: search.evidence,
            capability_missing: Some("llm_answering".to_owned()),
        })
    }

    async fn overview(&self, scope: &TenantScope) -> Result<KnowledgeOverview, AppError> {
        let project_id = Self::project_id(scope)?;
        let mut transaction = self.transaction(scope).await?;
        let source_count:i64=sqlx::query_scalar("SELECT count(*) FROM knowledge_sources WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3").bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project_id.as_uuid()).fetch_one(&mut *transaction).await.map_err(database_error)?;
        let fact_count:i64=sqlx::query_scalar("SELECT count(*) FROM knowledge_facts WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3").bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project_id.as_uuid()).fetch_one(&mut *transaction).await.map_err(database_error)?;
        let importing_count:i64=sqlx::query_scalar("SELECT count(*) FROM knowledge_import_jobs WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND status IN ('queued','running')").bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid()).bind(project_id.as_uuid()).fetch_one(&mut *transaction).await.map_err(database_error)?;
        let current = Self::current_release_in_transaction(&mut transaction, scope).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(KnowledgeOverview {
            source_count: source_count as u64,
            fact_count: fact_count as u64,
            importing_count: importing_count as u64,
            current_release_id: current.knowledge_release_id,
        })
    }
}

fn source_kind(value: SourceKind) -> &'static str {
    match value {
        SourceKind::File => "file",
        SourceKind::Url => "url",
        SourceKind::Text => "text",
        SourceKind::Object => "object",
        SourceKind::KnowledgeCollection => "knowledge_collection",
        SourceKind::Manual => "manual",
    }
}
fn purpose(value: KnowledgePurpose) -> &'static str {
    match value {
        KnowledgePurpose::Public => "public",
        KnowledgePurpose::Internal => "internal",
    }
}
fn parse_purpose(value: &str) -> Result<KnowledgePurpose, AppError> {
    match value {
        "public" => Ok(KnowledgePurpose::Public),
        "internal" => Ok(KnowledgePurpose::Internal),
        _ => Err(AppError::new(
            geo_domain::ErrorCode::Internal,
            "invalid knowledge purpose in database",
        )),
    }
}
fn parse_source_kind(value: &str) -> Result<SourceKind, AppError> {
    match value {
        "file" => Ok(SourceKind::File),
        "url" => Ok(SourceKind::Url),
        "text" => Ok(SourceKind::Text),
        "object" => Ok(SourceKind::Object),
        "knowledge_collection" => Ok(SourceKind::KnowledgeCollection),
        "manual" => Ok(SourceKind::Manual),
        _ => Err(AppError::new(
            geo_domain::ErrorCode::Internal,
            "invalid source kind in database",
        )),
    }
}
fn is_text(value: &str) -> bool {
    matches!(
        value.split(';').next().unwrap_or(value).trim(),
        "text/plain" | "text/markdown" | "text/x-markdown"
    )
}
fn source_from_row(
    row: &sqlx::postgres::PgRow,
    scope: &TenantScope,
    project_id: geo_domain::ProjectId,
) -> Result<Source, AppError> {
    Ok(Source {
        source_id: row.get("source_id"),
        operator_id: scope.operator_id,
        tenant_id: scope.tenant_id,
        project_id,
        revision: row.get("revision"),
        kind: parse_source_kind(&row.get::<String, _>("kind"))?,
        name: row.get("name"),
        purpose: parse_purpose(&row.get::<String, _>("purpose"))?,
        state: match row.get::<String, _>("state").as_str() {
            "active" => SourceState::Active,
            "removed" => SourceState::Removed,
            _ => {
                return Err(AppError::new(
                    geo_domain::ErrorCode::Internal,
                    "invalid source state in database",
                ));
            }
        },
        locator: row.get("locator"),
        current_version_id: row.get("current_version_id"),
        sync_enabled: row.get("sync_enabled"),
        next_sync_at: row.get("next_sync_at"),
        last_sync_at: row.get("last_sync_at"),
    })
}
fn version_from_row(
    row: &sqlx::postgres::PgRow,
    scope: &TenantScope,
    project_id: geo_domain::ProjectId,
) -> Result<SourceVersion, AppError> {
    Ok(SourceVersion {
        source_version_id: row.get("source_version_id"),
        operator_id: scope.operator_id,
        tenant_id: scope.tenant_id,
        project_id,
        source_id: row.get("source_id"),
        version: row.get("version"),
        object_id: row.get("object_id"),
        object_version: row.get("object_version"),
        content_sha256: row.get("content_sha256"),
        captured_at: row.get("captured_at"),
        original_url: row.get("original_url"),
        parent_version_id: row.get("parent_version_id"),
        parser_version: row.get("parser_version"),
        extraction_version: row.get("extraction_version"),
        created_at: row.get("created_at"),
    })
}
fn chunk_from_row(
    row: &sqlx::postgres::PgRow,
    scope: &TenantScope,
    project_id: geo_domain::ProjectId,
) -> Result<Chunk, AppError> {
    Ok(Chunk {
        chunk_id: row.get("chunk_id"),
        operator_id: scope.operator_id,
        tenant_id: scope.tenant_id,
        project_id,
        source_version_id: row.get("source_version_id"),
        ordinal: row.get("ordinal"),
        kind: geo_domain::ChunkKind::Paragraph,
        text: row.get("text"),
        text_hash: row.get("text_hash"),
        locator: serde_json::from_value(row.get::<Value, _>("locator"))
            .map_err(serialization_error)?,
        product_ids: serde_json::from_value(row.get::<Value, _>("product_ids"))
            .map_err(serialization_error)?,
        market: row.get("market"),
        language: row.get("language"),
        extraction_method: row.get("extraction_method"),
        confidence: row.get("confidence"),
    })
}
fn evidence_from_row(row: &sqlx::postgres::PgRow) -> Result<KnowledgeEvidence, AppError> {
    let text: String = row.get("text");
    Ok(KnowledgeEvidence {
        source_id: row.get("evidence_source_id"),
        source_version_id: row.get("source_version_id"),
        chunk_id: row.get("chunk_id"),
        source_name: row.get("evidence_source_name"),
        purpose: parse_purpose(&row.get::<String, _>("evidence_purpose"))?,
        locator: serde_json::from_value(row.get::<Value, _>("locator"))
            .map_err(serialization_error)?,
        quote: text.clone(),
        text,
    })
}
fn job_from_row(
    row: &sqlx::postgres::PgRow,
    scope: &TenantScope,
    project_id: geo_domain::ProjectId,
) -> Result<ImportJob, AppError> {
    Ok(ImportJob {
        import_job_id: row.get("import_job_id"),
        operator_id: scope.operator_id,
        tenant_id: scope.tenant_id,
        project_id,
        operation_id: row.get("operation_id"),
        source_id: row.get("source_id"),
        source_version_id: row.get("source_version_id"),
        stage: match row.get::<String, _>("stage").as_str() {
            "acquire" => ImportStage::Acquire,
            "parse" => ImportStage::Parse,
            "extract" => ImportStage::Extract,
            "index" => ImportStage::Index,
            "release" => ImportStage::Release,
            _ => {
                return Err(AppError::new(
                    geo_domain::ErrorCode::Internal,
                    "invalid import stage",
                ));
            }
        },
        status: match row.get::<String, _>("status").as_str() {
            "queued" => ImportStatus::Queued,
            "running" => ImportStatus::Running,
            "partial" => ImportStatus::Partial,
            "succeeded" => ImportStatus::Succeeded,
            "failed" => ImportStatus::Failed,
            "cancelled" => ImportStatus::Cancelled,
            _ => {
                return Err(AppError::new(
                    geo_domain::ErrorCode::Internal,
                    "invalid import status",
                ));
            }
        },
        attempt: row.get("attempt"),
        lease_until: row.get("lease_until"),
        input_hash: row.get("input_hash"),
        stage_output_refs: serde_json::from_value(row.get("stage_output_refs"))
            .map_err(serialization_error)?,
        completed_units: row.get("completed_units"),
        failed_units: row.get("failed_units"),
        errors: serde_json::from_value(row.get("errors")).map_err(serialization_error)?,
        resumed_from: row.get("resumed_from"),
    })
}
async fn release_from_row(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    project_id: geo_domain::ProjectId,
    row: &sqlx::postgres::PgRow,
) -> Result<KnowledgeRelease, AppError> {
    let id: Uuid = row.get("knowledge_release_id");
    let refs=sqlx::query_scalar("SELECT source_version_id FROM knowledge_release_source_versions WHERE knowledge_release_id=$1 ORDER BY source_version_id").bind(id).fetch_all(&mut **transaction).await.map_err(database_error)?;
    let facts=sqlx::query_as("SELECT fact_id,fact_revision FROM knowledge_release_facts WHERE knowledge_release_id=$1 ORDER BY fact_id").bind(id).fetch_all(&mut **transaction).await.map_err(database_error)?;
    Ok(KnowledgeRelease {
        knowledge_release_id: id,
        operator_id: scope.operator_id,
        tenant_id: scope.tenant_id,
        project_id,
        sequence: row.get("sequence"),
        previous_release_id: row.get("previous_release_id"),
        source_version_refs: refs,
        fact_revision_refs: facts,
        index_build_id: row.get("index_build_id"),
        pipeline_versions: row.get("pipeline_versions"),
        content_hash: row.get("content_hash"),
        coverage: serde_json::from_value(row.get("coverage")).map_err(serialization_error)?,
        created_at: row.get("created_at"),
    })
}
fn database_error(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        geo_domain::ErrorCode::DependencyUnavailable,
        format!("knowledge persistence is unavailable: {error}"),
    )
}
fn serialization_error(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        geo_domain::ErrorCode::Internal,
        format!("knowledge serialization failed: {error}"),
    )
}

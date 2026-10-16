use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ReviseSourceTextCommand, SourceTextBasis, SourceTextRevisionReceipt, SourceVersion,
    SourceVersionContent, SourceVersionRepresentation, TenantScope, knowledge_parser_version,
    parsed_knowledge_chunks, sha256_hex,
};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use super::{
    PgKnowledgeRepository, database_error, serialization_error, source_from_row, version_from_row,
};

pub(super) async fn content(
    repo: &PgKnowledgeRepository,
    scope: &TenantScope,
    source_id: Uuid,
    version_id: Uuid,
) -> Result<Option<SourceVersionContent>, AppError> {
    let project = PgKnowledgeRepository::project_id(scope)?;
    let mut tx = repo.transaction(scope).await?;
    let row = sqlx::query(
        "SELECT version.representation,version.content_sha256,version.object_id,
                version.object_version,body.media_type AS authored_media_type,body.body,
                object.detected_media_type,object.sha256 AS object_sha256,
                object.actual_size,object.object_version AS actual_object_version,
                blob.content AS object_bytes,blob.sha256 AS blob_sha256,
                blob.actual_size AS blob_size
         FROM knowledge_source_versions version
         LEFT JOIN knowledge_authored_text body ON body.source_version_id=version.source_version_id
           AND body.operator_id=version.operator_id AND body.tenant_id=version.tenant_id
           AND body.project_id=version.project_id AND body.source_id=version.source_id
         LEFT JOIN knowledge_stored_objects object ON object.object_id=version.object_id
           AND object.operator_id=version.operator_id AND object.tenant_id=version.tenant_id
           AND object.project_id=version.project_id
         LEFT JOIN knowledge_upload_sessions session ON session.committed_object_id=object.object_id
           AND session.operator_id=object.operator_id AND session.tenant_id=object.tenant_id
           AND session.project_id=object.project_id AND session.state='committed'
         LEFT JOIN knowledge_upload_blobs blob ON blob.upload_session_id=session.upload_session_id
         WHERE version.source_version_id=$1 AND version.source_id=$2
           AND version.operator_id=$3 AND version.tenant_id=$4 AND version.project_id=$5",
    )
    .bind(version_id)
    .bind(source_id)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?;
    let Some(row) = row else { return Ok(None) };
    let representation: String = row.get("representation");
    if representation == "authored_text" {
        let text: String = row.try_get("body").map_err(database_error)?;
        if sha256_hex(text.as_bytes()) != row.get::<String, _>("content_sha256") {
            return Err(AppError::conflict("authored text integrity check failed"));
        }
        return Ok(Some(SourceVersionContent {
            source_version_id: version_id,
            representation: SourceVersionRepresentation::AuthoredText,
            media_type: row.get("authored_media_type"),
            text,
            text_basis: SourceTextBasis::Exact,
        }));
    }
    if representation != "original" {
        return Err(AppError::conflict("unknown source representation"));
    }
    if let Some(media_type) = row.get::<Option<String>, _>("detected_media_type")
        && matches!(media_type.as_str(), "text/plain" | "text/markdown")
        && let Some(bytes) = row.get::<Option<Vec<u8>>, _>("object_bytes")
        && let Ok(text) = String::from_utf8(bytes.clone())
        && row.get::<Option<i64>, _>("object_version")
            == row.get::<Option<i64>, _>("actual_object_version")
        && bytes.len() as i64 == row.get::<Option<i64>, _>("actual_size").unwrap_or(-1)
        && bytes.len() as i64 == row.get::<Option<i64>, _>("blob_size").unwrap_or(-1)
        && sha256_hex(&bytes)
            == row
                .get::<Option<String>, _>("blob_sha256")
                .unwrap_or_default()
        && sha256_hex(&bytes)
            == row
                .get::<Option<String>, _>("object_sha256")
                .unwrap_or_default()
        && sha256_hex(&bytes) == row.get::<String, _>("content_sha256")
    {
        return Ok(Some(SourceVersionContent {
            source_version_id: version_id,
            representation: SourceVersionRepresentation::Original,
            media_type,
            text,
            text_basis: SourceTextBasis::Exact,
        }));
    }
    let chunks: Vec<String> = sqlx::query_scalar(
        "SELECT text FROM knowledge_chunks WHERE operator_id=$1 AND tenant_id=$2
         AND project_id=$3 AND source_version_id=$4 ORDER BY ordinal",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(version_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    Ok(Some(SourceVersionContent {
        source_version_id: version_id,
        representation: SourceVersionRepresentation::Original,
        media_type: "text/plain".to_owned(),
        text: chunks.join("\n\n"),
        text_basis: SourceTextBasis::Extracted,
    }))
}

pub(super) async fn revise(
    repo: &PgKnowledgeRepository,
    scope: &TenantScope,
    source_id: Uuid,
    expected_revision: i64,
    idempotency_key: &str,
    command: ReviseSourceTextCommand,
) -> Result<SourceTextRevisionReceipt, AppError> {
    let project = PgKnowledgeRepository::project_id(scope)?;
    command.validate()?;
    if idempotency_key.is_empty() {
        return Err(AppError::invalid_request(
            "Idempotency-Key must not be empty",
        ));
    }
    let request_hash = sha256_hex(
        &serde_json::to_vec(&(expected_revision, &command)).map_err(serialization_error)?,
    );
    let key_hash = sha256_hex(idempotency_key.as_bytes());
    let mut tx = repo.transaction(scope).await?;
    // A transaction advisory lock serializes both existing-receipt reads and
    // absent-key writes. Different keys then contend on the source row lock.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!(
            "knowledge.revision:{}:{source_id}:{key_hash}",
            scope.storage_key()
        ))
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    if let Some(row) = sqlx::query(
        "SELECT request_hash,acceptance FROM knowledge_import_receipts
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3
           AND action='source_text_revision' AND target_id=$4 AND client_item_id=$5 FOR UPDATE",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(source_id)
    .bind(&key_hash)
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?
    {
        if row.get::<String, _>("request_hash") != request_hash {
            return Err(
                AppError::conflict("idempotency key reused for a different request")
                    .with_details(json!({"reason":"idempotency_conflict"})),
            );
        }
        return serde_json::from_value(row.get::<Value, _>("acceptance"))
            .map_err(serialization_error);
    }
    let row = sqlx::query(
        "SELECT source_id,revision,kind,name,purpose,state,locator,current_version_id,
                sync_enabled,next_sync_at,last_sync_at
         FROM knowledge_sources WHERE source_id=$1 AND operator_id=$2 AND tenant_id=$3
           AND project_id=$4 FOR UPDATE",
    )
    .bind(source_id)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::not_found("knowledge source not found"))?;
    let mut source = source_from_row(&row, scope, project)?;
    let row = sqlx::query(
        "SELECT source_version_id,source_id,version,representation,object_id,object_version,
                content_sha256,captured_at,original_url,parent_version_id,parser_version,
                extraction_version,created_at
         FROM knowledge_source_versions
         WHERE source_version_id=$1 AND source_id=$2 AND operator_id=$3 AND tenant_id=$4
           AND project_id=$5",
    )
    .bind(command.base_version_id)
    .bind(source_id)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::not_found("source version not found"))?;
    let base = version_from_row(&row, scope, project)?;
    if source.revision != expected_revision
        || source.current_version_id != Some(base.source_version_id)
        || source.state != geo_domain::SourceState::Active
    {
        return Err(AppError::conflict("knowledge source revision changed")
            .with_details(json!({"reason":"source_revision_conflict"})));
    }
    let parsing: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM knowledge_import_jobs job
         WHERE job.operator_id=$1 AND job.tenant_id=$2 AND job.project_id=$3
           AND job.source_id=$4 AND job.status IN ('queued','running')
           AND (EXISTS(SELECT 1 FROM knowledge_pdf_parse_tasks task
                       WHERE task.import_job_id=job.import_job_id)
             OR EXISTS(SELECT 1 FROM knowledge_office_parse_tasks task
                       WHERE task.import_job_id=job.import_job_id)))",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(source_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    if parsing {
        return Err(AppError::conflict("source parse in progress")
            .with_details(json!({"reason":"source_parse_in_progress"})));
    }
    // Failed parse attempts may have reserved versions newer than the current
    // published version; the source row lock serializes this allocation.
    let next_version: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(version),0)+1 FROM knowledge_source_versions
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND source_id=$4",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(source_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    let now = Utc::now();
    let version_id = Uuid::new_v4();
    let chunks = parsed_knowledge_chunks(scope, version_id, &command.text, &command.media_type)?;
    let version = SourceVersion {
        source_version_id: version_id,
        operator_id: scope.operator_id,
        tenant_id: scope.tenant_id,
        project_id: project,
        source_id,
        version: next_version,
        representation: SourceVersionRepresentation::AuthoredText,
        object_id: None,
        object_version: None,
        content_sha256: sha256_hex(command.text.as_bytes()),
        captured_at: now,
        original_url: None,
        parent_version_id: Some(base.source_version_id),
        parser_version: knowledge_parser_version(&command.media_type).to_owned(),
        extraction_version: "authored-text-v1".to_owned(),
        created_at: now,
    };
    persist_authored_version(
        &mut tx,
        scope,
        &version,
        &command.media_type,
        &command.text,
        &chunks,
    )
    .await?;
    sqlx::query(
        "UPDATE knowledge_sources SET revision=revision+1,current_version_id=$1,updated_at=$2
         WHERE source_id=$3 AND operator_id=$4 AND tenant_id=$5 AND project_id=$6",
    )
    .bind(version_id)
    .bind(now)
    .bind(source_id)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    source.revision += 1;
    source.current_version_id = Some(version_id);
    let knowledge_release = PgKnowledgeRepository::create_release(&mut tx, scope).await?;
    let receipt = SourceTextRevisionReceipt {
        source,
        source_version: version,
        knowledge_release,
    };
    sqlx::query(
        "INSERT INTO knowledge_import_receipts
         (knowledge_import_receipt_id,operator_id,tenant_id,project_id,action,target_id,
          client_item_id,request_hash,acceptance)
         VALUES ($1,$2,$3,$4,'source_text_revision',$5,$6,$7,$8)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(source_id)
    .bind(key_hash)
    .bind(request_hash)
    .bind(serde_json::to_value(&receipt).map_err(serialization_error)?)
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    ready_event(
        &mut tx,
        scope,
        &receipt.source_version,
        receipt.source.revision,
        receipt.knowledge_release.knowledge_release_id,
        None,
    )
    .await?;
    tx.commit().await.map_err(database_error)?;
    Ok(receipt)
}

/// Writes only immutable text/version evidence; transaction ownership and source
/// concurrency checks remain with the calling workflow.
async fn persist_authored_version(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    version: &SourceVersion,
    media_type: &str,
    text: &str,
    chunks: &[geo_domain::Chunk],
) -> Result<(), AppError> {
    let project = PgKnowledgeRepository::project_id(scope)?;
    let source_id = version.source_id;
    let version_id = version.source_version_id;
    sqlx::query(
        "INSERT INTO knowledge_source_versions
         (source_version_id,operator_id,tenant_id,project_id,source_id,version,representation,
          content_sha256,captured_at,parent_version_id,parser_version,extraction_version,created_at)
         VALUES ($1,$2,$3,$4,$5,$6,'authored_text',$7,$8,$9,$10,$11,$12)",
    )
    .bind(version_id)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(source_id)
    .bind(version.version)
    .bind(&version.content_sha256)
    .bind(version.captured_at)
    .bind(version.parent_version_id)
    .bind(&version.parser_version)
    .bind(&version.extraction_version)
    .bind(version.created_at)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    sqlx::query(
        "INSERT INTO knowledge_authored_text
         (operator_id,tenant_id,project_id,source_id,source_version_id,media_type,body)
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(source_id)
    .bind(version_id)
    .bind(media_type)
    .bind(text)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    for chunk in chunks {
        sqlx::query(
            "INSERT INTO knowledge_chunks
             (chunk_id,operator_id,tenant_id,project_id,source_version_id,ordinal,kind,text,
              text_hash,locator,product_ids,market,language,extraction_method,confidence)
             VALUES ($1,$2,$3,$4,$5,$6,'paragraph',$7,$8,$9,'[]'::jsonb,$10,$11,$12,$13)",
        )
        .bind(chunk.chunk_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid())
        .bind(version_id)
        .bind(chunk.ordinal)
        .bind(&chunk.text)
        .bind(&chunk.text_hash)
        .bind(serde_json::to_value(&chunk.locator).map_err(serialization_error)?)
        .bind(&chunk.market)
        .bind(&chunk.language)
        .bind(&chunk.extraction_method)
        .bind(chunk.confidence)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    }
    Ok(())
}

async fn ready_event(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    version: &SourceVersion,
    revision: i64,
    release_id: Uuid,
    partner_input: Option<&PartnerTextSnapshot<'_>>,
) -> Result<(), AppError> {
    let project = PgKnowledgeRepository::project_id(scope)?;
    let mut payload = json!({"source_id":version.source_id,"source_version_id":version.source_version_id,
        "content_sha256":version.content_sha256,"knowledge_release_id":release_id});
    if let Some(input) = partner_input {
        payload["requested_purpose"] = json!(input.purpose);
        payload["effective_purpose"] = json!(input.effective_purpose);
        payload["effective_state"] = json!(input.effective_state);
    }
    sqlx::query(
        "INSERT INTO outbox_events
         (event_id,event_type,schema_version,operator_id,tenant_id,project_id,aggregate_id,
          aggregate_version,occurred_at,correlation_id,payload)
         VALUES ($1,'knowledge.source.version.ready',1,$2,$3,$4,$5,$6,$7,$8,$9)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(version.source_id)
    .bind(revision)
    .bind(version.created_at)
    .bind(release_id)
    .bind(payload)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    Ok(())
}

pub(crate) struct PartnerTextSnapshot<'a> {
    pub name: &'a str,
    /// Requested intent remains distinct from a newer intake restriction.
    pub purpose: geo_domain::KnowledgePurpose,
    pub effective_purpose: geo_domain::KnowledgePurpose,
    pub effective_state: geo_domain::SourceState,
    pub media_type: &'a str,
    pub text: &'a str,
    pub effective_at: DateTime<Utc>,
}

impl PgKnowledgeRepository {
    /// The caller owns the scoped transaction and locks any existing source
    /// before entry. Release locking is deliberately after that source lock.
    pub(crate) async fn apply_partner_text_in_transaction(
        tx: &mut Transaction<'_, Postgres>,
        scope: &TenantScope,
        source_id: Option<Uuid>,
        input: PartnerTextSnapshot<'_>,
    ) -> Result<geo_domain::PartnerKnowledgeRefs, AppError> {
        let project = Self::project_id(scope)?;
        ReviseSourceTextCommand {
            base_version_id: Uuid::nil(),
            media_type: input.media_type.to_owned(),
            text: input.text.to_owned(),
        }
        .validate()?;
        if input.name.trim().is_empty() {
            return Err(AppError::invalid_request("source name must not be blank"));
        }
        if input.purpose == geo_domain::KnowledgePurpose::Internal
            && input.effective_purpose == geo_domain::KnowledgePurpose::Public
        {
            return Err(AppError::invalid_request(
                "effective purpose cannot widen requested purpose",
            ));
        }
        let effective_state = match input.effective_state {
            geo_domain::SourceState::Active => "active",
            geo_domain::SourceState::Removed => "removed",
        };
        let now = Utc::now();
        let existing = source_id.is_some();
        let source_id = source_id.unwrap_or_else(Uuid::new_v4);
        let (parent_version_id, revision) = if existing {
            sqlx::query_as::<_, (Option<Uuid>, i64)>(
                "SELECT current_version_id,revision FROM knowledge_sources
                 WHERE source_id=$1 AND operator_id=$2 AND tenant_id=$3 AND project_id=$4
                 FOR UPDATE",
            )
            .bind(source_id)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project.as_uuid())
            .fetch_optional(&mut **tx)
            .await
            .map_err(database_error)?
            .ok_or_else(|| AppError::not_found("knowledge source not found"))?
        } else {
            sqlx::query(
                "INSERT INTO knowledge_sources
                 (source_id,operator_id,tenant_id,project_id,revision,kind,name,purpose,state,locator,current_version_id,sync_enabled)
                 VALUES ($1,$2,$3,$4,1,'text',$5,$6,$7,'{\"kind\":\"inline_text\"}'::jsonb,NULL,false)",
            )
            .bind(source_id).bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid()).bind(project.as_uuid())
            .bind(input.name.trim()).bind(super::purpose(input.effective_purpose)).bind(effective_state)
            .execute(&mut **tx).await.map_err(database_error)?;
            (None, 1)
        };
        let next_version: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(version),0)+1 FROM knowledge_source_versions
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND source_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid())
        .bind(source_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(database_error)?;
        let version_id = Uuid::new_v4();
        let chunks = parsed_knowledge_chunks(scope, version_id, input.text, input.media_type)?;
        let version = SourceVersion {
            source_version_id: version_id,
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id: project,
            source_id,
            version: next_version,
            representation: SourceVersionRepresentation::AuthoredText,
            object_id: None,
            object_version: None,
            content_sha256: sha256_hex(input.text.as_bytes()),
            captured_at: input.effective_at,
            original_url: None,
            parent_version_id,
            parser_version: knowledge_parser_version(input.media_type).to_owned(),
            extraction_version: "authored-text-v1".to_owned(),
            created_at: now,
        };
        persist_authored_version(tx, scope, &version, input.media_type, input.text, &chunks)
            .await?;
        let revision = revision + i64::from(existing);
        sqlx::query(
            "UPDATE knowledge_sources SET revision=$1,current_version_id=$2,name=$3,
             purpose=$4,state=$10,updated_at=$5
             WHERE source_id=$6 AND operator_id=$7 AND tenant_id=$8 AND project_id=$9",
        )
        .bind(revision)
        .bind(version_id)
        .bind(input.name.trim())
        .bind(super::purpose(input.effective_purpose))
        .bind(now)
        .bind(source_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid())
        .bind(effective_state)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
        let operation_id = Uuid::new_v4();
        let job_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO operations (operation_id,operator_id,tenant_id,project_id,kind,status,created_at,updated_at)
             VALUES ($1,$2,$3,$4,'knowledge.import','succeeded',$5,$5)",
        )
        .bind(operation_id).bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid()).bind(project.as_uuid()).bind(now)
        .execute(&mut **tx).await.map_err(database_error)?;
        sqlx::query(
            "INSERT INTO knowledge_import_jobs
             (import_job_id,operator_id,tenant_id,project_id,operation_id,source_id,source_version_id,
              stage,status,attempt,input_hash,stage_output_refs,completed_units,failed_units,errors)
             VALUES ($1,$2,$3,$4,$5,$6,$7,'release','succeeded',1,$8,$9,$10,0,'[]'::jsonb)",
        )
        .bind(job_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid()).bind(operation_id).bind(source_id).bind(version_id)
        .bind(&version.content_sha256)
        .bind(json!(chunks.iter().map(|chunk| chunk.chunk_id.to_string()).collect::<Vec<_>>()))
        .bind(chunks.len() as i32)
        .execute(&mut **tx).await.map_err(database_error)?;
        let release = Self::create_release(tx, scope).await?;
        sqlx::query("UPDATE operations SET result=$1 WHERE operation_id=$2")
            .bind(json!({"source_id":source_id,"source_version_id":version_id,
                "knowledge_release_id":release.knowledge_release_id}))
            .bind(operation_id)
            .execute(&mut **tx)
            .await
            .map_err(database_error)?;
        ready_event(
            tx,
            scope,
            &version,
            revision,
            release.knowledge_release_id,
            Some(&input),
        )
        .await?;
        Ok(geo_domain::PartnerKnowledgeRefs {
            source_id: Some(source_id),
            source_version_id: Some(version_id),
            import_job_id: Some(job_id),
            knowledge_release_id: Some(release.knowledge_release_id),
        })
    }

    /// Tightens live authority at intake without creating versions or parse jobs.
    /// The caller prelocks all sources before the first call in a batch, preserving
    /// source-before-release lock ordering. No transaction is started or committed.
    pub(crate) async fn restrict_partner_source_in_transaction(
        tx: &mut Transaction<'_, Postgres>,
        scope: &TenantScope,
        source_id: Uuid,
        purpose: Option<geo_domain::KnowledgePurpose>,
        remove: bool,
    ) -> Result<(), AppError> {
        if purpose == Some(geo_domain::KnowledgePurpose::Public) {
            return Err(AppError::invalid_request(
                "intake restrictions cannot grant public use",
            ));
        }
        let project = Self::project_id(scope)?;
        let row = sqlx::query(
            "SELECT purpose,state FROM knowledge_sources
             WHERE source_id=$1 AND operator_id=$2 AND tenant_id=$3 AND project_id=$4 FOR UPDATE",
        )
        .bind(source_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid())
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?
        .ok_or_else(|| AppError::not_found("knowledge source not found"))?;
        let internal = purpose == Some(geo_domain::KnowledgePurpose::Internal);
        if (!internal || row.get::<String, _>("purpose") == "internal")
            && (!remove || row.get::<String, _>("state") == "removed")
        {
            return Ok(());
        }
        let changed = sqlx::query(
            "UPDATE knowledge_sources SET
             purpose=CASE WHEN $5 THEN 'internal' ELSE purpose END,
             state=CASE WHEN $6 THEN 'removed' ELSE state END,
             revision=revision+1,updated_at=now()
             WHERE source_id=$1 AND operator_id=$2 AND tenant_id=$3 AND project_id=$4
             RETURNING revision,current_version_id,purpose,state",
        )
        .bind(source_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid())
        .bind(internal)
        .bind(remove)
        .fetch_one(&mut **tx)
        .await
        .map_err(database_error)?;
        let release = Self::create_release(tx, scope).await?;
        sqlx::query(
            "INSERT INTO outbox_events
             (event_id,event_type,schema_version,operator_id,tenant_id,project_id,aggregate_id,
              aggregate_version,occurred_at,correlation_id,payload)
             VALUES ($1,'knowledge.source.restricted',1,$2,$3,$4,$5,$6,now(),$7,$8)",
        )
        .bind(Uuid::new_v4()).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid()).bind(source_id).bind(changed.get::<i64,_>("revision"))
        .bind(release.knowledge_release_id)
        .bind(json!({"source_id":source_id,"source_version_id":changed.get::<Option<Uuid>,_>("current_version_id"),
            "knowledge_release_id":release.knowledge_release_id,
            "effective_purpose":changed.get::<String,_>("purpose"),"effective_state":changed.get::<String,_>("state"),
            "external_withdrawal_confirmed":false}))
        .execute(&mut **tx).await.map_err(database_error)?;
        Ok(())
    }

    /// Retains immutable versions and evidence. This invalidates local use only;
    /// it makes no assertion that any externally published asset was withdrawn.
    pub(crate) async fn revoke_partner_source_in_transaction(
        tx: &mut Transaction<'_, Postgres>,
        scope: &TenantScope,
        source_id: Uuid,
    ) -> Result<geo_domain::PartnerKnowledgeRefs, AppError> {
        let project = Self::project_id(scope)?;
        let (version_id, revision) = sqlx::query_as::<_, (Option<Uuid>, i64)>(
            "UPDATE knowledge_sources SET state='removed',revision=revision+1,updated_at=now()
             WHERE source_id=$1 AND operator_id=$2 AND tenant_id=$3 AND project_id=$4
             RETURNING current_version_id,revision",
        )
        .bind(source_id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid())
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?
        .ok_or_else(|| AppError::not_found("knowledge source not found"))?;
        let release = Self::create_release(tx, scope).await?;
        sqlx::query(
            "INSERT INTO outbox_events
             (event_id,event_type,schema_version,operator_id,tenant_id,project_id,aggregate_id,
              aggregate_version,occurred_at,correlation_id,payload)
             VALUES ($1,'knowledge.source.revoked',1,$2,$3,$4,$5,$6,now(),$7,$8)",
        )
        .bind(Uuid::new_v4()).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid()).bind(source_id).bind(revision).bind(release.knowledge_release_id)
        .bind(json!({"source_id":source_id,"source_version_id":version_id,
            "knowledge_release_id":release.knowledge_release_id,"external_withdrawal_confirmed":false}))
        .execute(&mut **tx).await.map_err(database_error)?;
        Ok(geo_domain::PartnerKnowledgeRefs {
            source_id: Some(source_id),
            source_version_id: version_id,
            import_job_id: None,
            knowledge_release_id: Some(release.knowledge_release_id),
        })
    }
}

#[cfg(test)]
mod partner_transaction_tests {
    use super::*;
    use crate::{Database, DatabaseConfig, PgProjectRepository};
    use geo_domain::{KnowledgePurpose, ProjectCreate, ProjectRepository, ProjectSettings};

    #[tokio::test]
    #[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
    async fn caller_rollback_removes_all_partner_knowledge_writes() {
        let url = std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database");
        let database = Database::connect_and_migrate(&DatabaseConfig::from_url(url).unwrap())
            .await
            .unwrap();
        let operator = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO operators(operator_id,slug,display_name) VALUES($1,$2,'Synthetic')",
        )
        .bind(operator)
        .bind(format!("rollback-{operator}"))
        .execute(database.pool())
        .await
        .unwrap();
        sqlx::query("INSERT INTO tenants(tenant_id,operator_id,slug,display_name) VALUES($1,$2,$3,'Synthetic')")
            .bind(tenant).bind(operator).bind(format!("rollback-{tenant}"))
            .execute(database.pool()).await.unwrap();
        let project = PgProjectRepository::from_database(&database)
            .create(
                &TenantScope::new(operator.into(), tenant.into(), None),
                ProjectCreate {
                    slug: Some(format!("rollback-{tenant}")),
                    display_name: "Synthetic".into(),
                    settings: ProjectSettings::default(),
                },
            )
            .await
            .unwrap();
        let scope = TenantScope::new(operator.into(), tenant.into(), Some(project.id));
        let repo = PgKnowledgeRepository::from_database(&database);
        let mut tx = repo.transaction(&scope).await.unwrap();
        let refs = PgKnowledgeRepository::apply_partner_text_in_transaction(
            &mut tx,
            &scope,
            None,
            PartnerTextSnapshot {
                name: "Synthetic",
                purpose: KnowledgePurpose::Public,
                effective_purpose: KnowledgePurpose::Public,
                effective_state: geo_domain::SourceState::Active,
                media_type: "text/plain",
                text: "Synthetic transaction content.",
                effective_at: Utc::now(),
            },
        )
        .await
        .unwrap();
        PgKnowledgeRepository::restrict_partner_source_in_transaction(
            &mut tx,
            &scope,
            refs.source_id.unwrap(),
            Some(KnowledgePurpose::Internal),
            true,
        )
        .await
        .unwrap();
        let restricted_revision: i64 =
            sqlx::query_scalar("SELECT revision FROM knowledge_sources WHERE source_id=$1")
                .bind(refs.source_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        let restricted_release =
            PgKnowledgeRepository::current_release_in_transaction(&mut tx, &scope)
                .await
                .unwrap();
        PgKnowledgeRepository::restrict_partner_source_in_transaction(
            &mut tx,
            &scope,
            refs.source_id.unwrap(),
            Some(KnowledgePurpose::Internal),
            true,
        )
        .await
        .unwrap();
        let unchanged_revision: i64 =
            sqlx::query_scalar("SELECT revision FROM knowledge_sources WHERE source_id=$1")
                .bind(refs.source_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(restricted_revision, unchanged_revision);
        assert_eq!(
            restricted_release.knowledge_release_id,
            PgKnowledgeRepository::current_release_in_transaction(&mut tx, &scope)
                .await
                .unwrap()
                .knowledge_release_id
        );
        let revoked = PgKnowledgeRepository::revoke_partner_source_in_transaction(
            &mut tx,
            &scope,
            refs.source_id.unwrap(),
        )
        .await
        .unwrap();
        assert!(revoked.knowledge_release_id.is_some());
        tx.rollback().await.unwrap();
        for table in [
            "knowledge_sources",
            "knowledge_source_versions",
            "knowledge_authored_text",
            "knowledge_chunks",
            "knowledge_import_jobs",
            "knowledge_releases",
            "knowledge_current_releases",
            "operations",
            "outbox_events",
        ] {
            let count: i64 = sqlx::query_scalar(&format!(
                "SELECT count(*) FROM {table} WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3"))
                .bind(operator).bind(tenant).bind(project.id.as_uuid())
                .fetch_one(database.pool()).await.unwrap();
            assert_eq!(count, 0, "caller rollback left rows in {table}");
        }
    }
}

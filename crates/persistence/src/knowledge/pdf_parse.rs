//! Durable PDF parse ledger. All writes are scoped and fenced by database time.
use super::*;
use geo_domain::{
    PDF_MAX_DOCUMENT_TEXT_BYTES, PdfDocumentManifest, PdfPageResult, PdfPageText, PdfParseCursor,
    PdfParseInput, PdfParseJobRef, PdfParseLease, pdf_page_chunks,
};

fn configured(repo: &PgKnowledgeRepository) -> Result<(), AppError> {
    if repo.pdf_parser_profile.is_none() {
        return Err(AppError::capability_missing("PDF parser is not configured"));
    }
    Ok(())
}

fn lease_error() -> AppError {
    AppError::conflict("PDF parse lease expired, fenced, or source is unavailable")
}

fn lease_seconds(seconds: i64) -> Result<i64, AppError> {
    if !(1..=300).contains(&seconds) {
        return Err(AppError::invalid_request(
            "invalid PDF parse lease duration",
        ));
    }
    Ok(seconds)
}

async fn locked_task(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    lease: &PdfParseLease,
) -> Result<sqlx::postgres::PgRow, AppError> {
    let project = PgKnowledgeRepository::project_id(scope)?;
    let job = sqlx::query(
        "SELECT import_job_id FROM knowledge_import_jobs
         WHERE import_job_id=$1 AND operator_id=$2 AND tenant_id=$3 AND project_id=$4
           AND status='running' AND lease_until>clock_timestamp()
         FOR UPDATE",
    )
    .bind(lease.job_id)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    if job.is_none() {
        return Err(lease_error());
    }
    sqlx::query(
        "SELECT task.source_id,task.source_version_id,task.object_id,task.object_version,
                task.input_sha256,task.parser_profile,task.manifest_schema,task.page_count,
                task.released_id,job.operation_id,job.status,job.attempt,job.resumed_from,
                source.state,source.purpose,source.name,source.kind,source.revision,
                clock_timestamp() AS db_now
         FROM knowledge_pdf_parse_tasks task
         JOIN knowledge_import_jobs job ON job.import_job_id=task.import_job_id
         JOIN knowledge_sources source ON source.source_id=task.source_id
         WHERE task.import_job_id=$1 AND task.operator_id=$2 AND task.tenant_id=$3 AND task.project_id=$4
           AND task.lease_id=$5 AND task.fencing_token=$6
           AND job.status='running' AND job.lease_until > clock_timestamp()
           AND source.state='active'
         FOR UPDATE OF task,source",
    )
    .bind(lease.job_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid()).bind(lease.lease_id).bind(lease.fencing_token)
    .fetch_optional(&mut **tx).await.map_err(database_error)?
    .ok_or_else(lease_error)
}

pub(super) async fn candidates(
    repo: &PgKnowledgeRepository,
    after: Option<PdfParseCursor>,
    limit: usize,
) -> Result<Vec<PdfParseJobRef>, AppError> {
    configured(repo)?;
    let rows = sqlx::query(
        "SELECT task.operator_id,task.tenant_id,task.project_id,task.import_job_id,task.created_at
         FROM knowledge_pdf_parse_tasks task
         JOIN knowledge_import_jobs job ON job.import_job_id=task.import_job_id
         JOIN knowledge_sources source ON source.source_id=task.source_id
         WHERE (job.status='queued' OR (job.status='running' AND job.lease_until <= clock_timestamp()))
           AND source.state='active'
           AND ($1::timestamptz IS NULL OR (task.created_at,task.import_job_id) > ($1,$2))
         ORDER BY task.created_at,task.import_job_id LIMIT $3",
    )
    .bind(after.map(|c| c.created_at))
    .bind(after.map(|c| c.job_id).unwrap_or(Uuid::nil()))
    .bind(limit.clamp(1, 200) as i64)
    .fetch_all(&repo.pool).await.map_err(database_error)?;
    Ok(rows
        .into_iter()
        .map(|row| PdfParseJobRef {
            scope: TenantScope {
                operator_id: row.get::<Uuid, _>("operator_id").into(),
                tenant_id: row.get::<Uuid, _>("tenant_id").into(),
                project_id: Some(row.get::<Uuid, _>("project_id").into()),
            },
            job_id: row.get("import_job_id"),
            created_at: row.get("created_at"),
        })
        .collect())
}

pub(super) async fn claim(
    repo: &PgKnowledgeRepository,
    scope: &TenantScope,
    job_id: Uuid,
    lease_id: Uuid,
    seconds: i64,
) -> Result<Option<PdfParseLease>, AppError> {
    configured(repo)?;
    let seconds = lease_seconds(seconds)?;
    let project = PgKnowledgeRepository::project_id(scope)?;
    let mut tx = repo.transaction(scope).await?;
    let row = sqlx::query(
        "UPDATE knowledge_import_jobs job
         SET status='running',attempt=attempt+1,
             lease_until=clock_timestamp()+make_interval(secs=>$5::int),updated_at=clock_timestamp()
         FROM knowledge_pdf_parse_tasks task, knowledge_sources source
         WHERE job.import_job_id=$1 AND job.operator_id=$2 AND job.tenant_id=$3 AND job.project_id=$4
           AND task.import_job_id=job.import_job_id AND source.source_id=job.source_id
           AND source.state='active'
           AND (job.status='queued' OR (job.status='running' AND job.lease_until <= clock_timestamp()))
         RETURNING job.lease_until",
    )
    .bind(job_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid()).bind(seconds as i32)
    .fetch_optional(&mut *tx).await.map_err(database_error)?;
    let Some(row) = row else {
        tx.commit().await.map_err(database_error)?;
        return Ok(None);
    };
    let expires_at = row.get("lease_until");
    let epoch: i64 = sqlx::query_scalar(
        "UPDATE knowledge_pdf_parse_tasks
         SET lease_id=$2,fencing_token=fencing_token+1
         WHERE import_job_id=$1 RETURNING fencing_token",
    )
    .bind(job_id)
    .bind(lease_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    sqlx::query("UPDATE operations SET status='running',updated_at=clock_timestamp() WHERE operation_id=(SELECT operation_id FROM knowledge_import_jobs WHERE import_job_id=$1)")
        .bind(job_id).execute(&mut *tx).await.map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    Ok(Some(PdfParseLease {
        job_id,
        lease_id,
        fencing_token: epoch,
        expires_at,
    }))
}

pub(super) async fn renew(
    repo: &PgKnowledgeRepository,
    scope: &TenantScope,
    lease: &PdfParseLease,
    seconds: i64,
) -> Result<Option<PdfParseLease>, AppError> {
    configured(repo)?;
    let seconds = lease_seconds(seconds)?;
    let mut tx = repo.transaction(scope).await?;
    if locked_task(&mut tx, scope, lease).await.is_err() {
        return Ok(None);
    }
    let until = sqlx::query_scalar::<_, chrono::DateTime<Utc>>(
        "UPDATE knowledge_import_jobs SET lease_until=clock_timestamp()+make_interval(secs=>$2::int),
             updated_at=clock_timestamp() WHERE import_job_id=$1 RETURNING lease_until",
    ).bind(lease.job_id).bind(seconds as i32)
    .fetch_one(&mut *tx).await.map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    Ok(Some(PdfParseLease {
        expires_at: until,
        ..lease.clone()
    }))
}

pub(super) async fn input(
    repo: &PgKnowledgeRepository,
    scope: &TenantScope,
    lease: &PdfParseLease,
) -> Result<PdfParseInput, AppError> {
    configured(repo)?;
    let mut tx = repo.transaction(scope).await?;
    let task = locked_task(&mut tx, scope, lease).await?;
    let project = PgKnowledgeRepository::project_id(scope)?;
    let object = sqlx::query(
        "SELECT object.sha256,object.actual_size,object.object_version,object.detected_media_type,
                blob.sha256 AS blob_hash,blob.actual_size AS blob_size,blob.content
         FROM knowledge_stored_objects object
         JOIN knowledge_upload_sessions session ON session.committed_object_id=object.object_id
         JOIN knowledge_upload_blobs blob ON blob.upload_session_id=session.upload_session_id
         WHERE object.object_id=$1 AND object.operator_id=$2 AND object.tenant_id=$3
           AND object.project_id=$4 AND object.state='committed' AND session.state='committed'",
    )
    .bind(task.get::<Uuid, _>("object_id"))
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::conflict("PDF source object is unavailable"))?;
    let bytes: Vec<u8> = object.get("content");
    let hash: String = task.get("input_sha256");
    let size: i64 = object.get("actual_size");
    if object.get::<String, _>("sha256") != hash
        || object.get::<String, _>("blob_hash") != hash
        || task.get::<i64, _>("object_version") != object.get::<i64, _>("object_version")
        || object.get::<i64, _>("blob_size") != size
        || bytes.len() as i64 != size
        || sha256_hex(&bytes) != hash
        || object.get::<String, _>("detected_media_type") != "application/pdf"
    {
        return Err(AppError::conflict(
            "PDF source object changed after acceptance",
        ));
    }
    let successful_pages = sqlx::query_scalar::<_, i32>(
        "SELECT page FROM knowledge_pdf_parse_pages WHERE import_job_id=$1 AND status='succeeded' ORDER BY page",
    ).bind(lease.job_id).fetch_all(&mut *tx).await.map_err(database_error)?
    .into_iter().map(|page| page as u32).collect();
    let manifest = task
        .get::<Option<i32>, _>("page_count")
        .map(|page_count| PdfDocumentManifest {
            schema_version: task.get("manifest_schema"),
            input_sha256: hash.clone(),
            parser_version: task.get("parser_profile"),
            page_count: page_count as u32,
        });
    let input = PdfParseInput {
        bytes,
        input_sha256: hash,
        media_type: object.get("detected_media_type"),
        parser_profile: task.get("parser_profile"),
        successful_pages,
        manifest,
    };
    tx.commit().await.map_err(database_error)?;
    Ok(input)
}

pub(super) async fn manifest(
    repo: &PgKnowledgeRepository,
    scope: &TenantScope,
    lease: &PdfParseLease,
    manifest: PdfDocumentManifest,
) -> Result<(), AppError> {
    configured(repo)?;
    let mut tx = repo.transaction(scope).await?;
    let task = locked_task(&mut tx, scope, lease).await?;
    manifest.validate(
        &task.get::<String, _>("input_sha256"),
        &task.get::<String, _>("parser_profile"),
    )?;
    if let Some(existing) = task.get::<Option<i32>, _>("page_count") {
        if existing != manifest.page_count as i32
            || task.get::<String, _>("manifest_schema") != manifest.schema_version
        {
            return Err(AppError::conflict("PDF manifest is immutable"));
        }
    } else {
        sqlx::query("UPDATE knowledge_pdf_parse_tasks SET manifest_schema=$2,page_count=$3 WHERE import_job_id=$1")
            .bind(lease.job_id).bind(&manifest.schema_version).bind(manifest.page_count as i32)
            .execute(&mut *tx).await.map_err(database_error)?;
    }
    tx.commit().await.map_err(database_error)?;
    Ok(())
}

pub(super) async fn page(
    repo: &PgKnowledgeRepository,
    scope: &TenantScope,
    lease: &PdfParseLease,
    result: PdfPageResult,
) -> Result<(), AppError> {
    configured(repo)?;
    let mut tx = repo.transaction(scope).await?;
    let task = locked_task(&mut tx, scope, lease).await?;
    let count: Option<i32> = task.get("page_count");
    result.validate(
        count.ok_or_else(|| AppError::conflict("PDF manifest is not recorded"))? as u32,
    )?;
    if matches!(&result, PdfPageResult::Success { text,.. } if text.trim().is_empty()) {
        return Err(AppError::invalid_request(
            "PDF whitespace-only page is not extracted text",
        ));
    }
    let page = result.page() as i32;
    let row = sqlx::query(
        "SELECT status,text_sha256,error_code,octet_length(text) AS previous_size
         FROM knowledge_pdf_parse_pages
         WHERE import_job_id=$1 AND page=$2",
    )
    .bind(lease.job_id)
    .bind(page)
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?;
    let result = if let PdfPageResult::Success { page, ref text } = result {
        let stored: i64 = sqlx::query_scalar(
            "SELECT COALESCE(sum(octet_length(text)),0)::bigint FROM knowledge_pdf_parse_pages
                 WHERE import_job_id=$1 AND status='succeeded'",
        )
        .bind(lease.job_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
        let previous_size = row
            .as_ref()
            .and_then(|existing| existing.get::<Option<i32>, _>("previous_size"))
            .unwrap_or(0) as i64;
        if stored - previous_size + text.len() as i64 > PDF_MAX_DOCUMENT_TEXT_BYTES as i64 {
            PdfPageResult::Failure {
                page,
                code: "page_limit".to_owned(),
            }
        } else {
            result
        }
    } else {
        result
    };
    let project = PgKnowledgeRepository::project_id(scope)?;
    let (status, text, code, hash) = match result {
        PdfPageResult::Success { text, .. } => {
            let hash = sha256_hex(text.as_bytes());
            ("succeeded", Some(text), None, Some(hash))
        }
        PdfPageResult::Failure { code, .. } => ("failed", None, Some(code), None),
    };
    if let Some(row) = row {
        if row.get::<String, _>("status") != status
            || row.get::<Option<String>, _>("text_sha256") != hash
            || row.get::<Option<String>, _>("error_code") != code
        {
            if row.get::<String, _>("status") == "succeeded" {
                return Err(AppError::conflict("successful PDF page is immutable"));
            }
            sqlx::query(
                "UPDATE knowledge_pdf_parse_pages
                 SET status=$3,text=$4,error_code=$5,text_sha256=$6,updated_at=clock_timestamp()
                 WHERE import_job_id=$1 AND page=$2 AND status='failed'",
            )
            .bind(lease.job_id)
            .bind(page)
            .bind(status)
            .bind(text)
            .bind(code)
            .bind(hash)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        }
    } else {
        sqlx::query(
            "INSERT INTO knowledge_pdf_parse_pages
             (import_job_id,operator_id,tenant_id,project_id,page,status,text,error_code,text_sha256)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        ).bind(lease.job_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid()).bind(page).bind(status).bind(text).bind(code).bind(hash)
        .execute(&mut *tx).await.map_err(database_error)?;
    }
    let counts = sqlx::query(
        "SELECT count(*) FILTER (WHERE status='succeeded')::integer AS completed,
                count(*) FILTER (WHERE status='failed')::integer AS failed
         FROM knowledge_pdf_parse_pages WHERE import_job_id=$1",
    )
    .bind(lease.job_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    let error_rows = sqlx::query(
        "SELECT page,error_code FROM knowledge_pdf_parse_pages
         WHERE import_job_id=$1 AND status='failed' ORDER BY page",
    )
    .bind(lease.job_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(database_error)?;
    let errors = error_rows
        .into_iter()
        .map(
            |row| json!({"page":row.get::<i32,_>("page"),"code":row.get::<String,_>("error_code")}),
        )
        .collect::<Vec<_>>();
    sqlx::query(
        "UPDATE knowledge_import_jobs
         SET completed_units=$2,failed_units=$3,errors=$4,updated_at=clock_timestamp()
         WHERE import_job_id=$1",
    )
    .bind(lease.job_id)
    .bind(counts.get::<i32, _>("completed"))
    .bind(counts.get::<i32, _>("failed"))
    .bind(json!(errors))
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    Ok(())
}

pub(super) async fn finish(
    repo: &PgKnowledgeRepository,
    scope: &TenantScope,
    lease: &PdfParseLease,
    terminal_error: Option<&str>,
) -> Result<ImportAcceptance, AppError> {
    configured(repo)?;
    if let Some(code) = terminal_error
        && !matches!(
            code,
            "invalid_pdf" | "encrypted_pdf" | "parse_failed" | "page_limit"
        )
    {
        return Err(AppError::invalid_request(
            "invalid PDF document failure code",
        ));
    }
    let project = PgKnowledgeRepository::project_id(scope)?;
    let mut tx = repo.transaction(scope).await?;
    let task = locked_task(&mut tx, scope, lease).await?;
    let page_count: Option<i32> = task.get("page_count");
    if terminal_error.is_none() && page_count.is_none() {
        return Err(AppError::conflict("PDF manifest is not recorded"));
    }
    let rows = sqlx::query(
        "SELECT page,status,text,error_code FROM knowledge_pdf_parse_pages
         WHERE import_job_id=$1 ORDER BY page",
    )
    .bind(lease.job_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(database_error)?;
    let success = rows
        .iter()
        .filter(|r| r.get::<String, _>("status") == "succeeded")
        .count();
    let missing = if let Some(page_count) = page_count {
        if terminal_error.is_none() && rows.len() != page_count as usize {
            return Err(AppError::conflict(
                "PDF page results do not cover the manifest",
            ));
        }
        page_count as usize - rows.len()
    } else {
        0
    };
    let failed = rows.len() - success + missing + usize::from(terminal_error.is_some());
    let partial = success > 0 && failed > 0;
    let job_status = if success == 0 || terminal_error.is_some() {
        "failed"
    } else if partial {
        "partial"
    } else {
        "succeeded"
    };
    let version_id: Uuid = task.get("source_version_id");
    let source_id: Uuid = task.get("source_id");
    if job_status != "failed" {
        // Only sealed page results generate chunks; the original extracted page
        // text and precise page/character locators remain available in the ledger.
        let mut ordinal: i32 = 0;
        for row in &rows {
            if row.get::<String, _>("status") != "succeeded" {
                continue;
            }
            let page = PdfPageText {
                page: row.get::<i32, _>("page") as u32,
                text: row.get("text"),
            };
            for chunk in pdf_page_chunks(scope, version_id, &page, ordinal)? {
                sqlx::query(
                    "INSERT INTO knowledge_chunks
                     (chunk_id,operator_id,tenant_id,project_id,source_version_id,ordinal,kind,
                      text,text_hash,locator,product_ids,market,language,extraction_method,confidence)
                     VALUES ($1,$2,$3,$4,$5,$6,'paragraph',$7,$8,$9,'[]'::jsonb,NULL,NULL,$10,$11)",
                )
                .bind(chunk.chunk_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
                .bind(project.as_uuid()).bind(version_id).bind(chunk.ordinal).bind(&chunk.text)
                .bind(&chunk.text_hash)
                .bind(serde_json::to_value(&chunk.locator).map_err(serialization_error)?)
                .bind(&chunk.extraction_method).bind(chunk.confidence)
                .execute(&mut *tx).await.map_err(database_error)?;
                ordinal += 1;
            }
        }
    }
    let errors: Vec<Value> = rows
        .iter()
        .filter_map(|row| {
            let code: Option<String> = row.get("error_code");
            code.map(|code| json!({"page":row.get::<i32,_>("page"),"code":code}))
        })
        .chain(terminal_error.map(|code| json!({"code":code})))
        .collect();
    sqlx::query(
        "UPDATE knowledge_import_jobs SET status=$2,stage=$3,completed_units=$4,failed_units=$5,
         errors=$6,lease_until=NULL,updated_at=clock_timestamp() WHERE import_job_id=$1",
    )
    .bind(lease.job_id)
    .bind(job_status)
    .bind(if job_status == "failed" {
        "parse"
    } else {
        "release"
    })
    .bind(success as i32)
    .bind(failed as i32)
    .bind(json!(&errors))
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    if job_status != "failed" {
        sqlx::query(
            "UPDATE knowledge_sources SET current_version_id=$2,revision=revision+1,
             updated_at=clock_timestamp() WHERE source_id=$1",
        )
        .bind(source_id)
        .bind(version_id)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    }
    let release = if job_status == "failed" {
        None
    } else {
        Some(PgKnowledgeRepository::create_release(&mut tx, scope).await?)
    };
    if let Some(ref release) = release {
        sqlx::query("UPDATE knowledge_pdf_parse_tasks SET released_id=$2 WHERE import_job_id=$1")
            .bind(lease.job_id)
            .bind(release.knowledge_release_id)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
    }
    let operation_id: Uuid = task.get("operation_id");
    let error = terminal_error
        .map(|code| AppError::invalid_request(format!("PDF parse failed: {code}")))
        .or_else(|| {
            if success == 0 {
                Some(AppError::invalid_request("no PDF pages were extracted"))
            } else {
                None
            }
        });
    let result = json!({
        "source_id":source_id,"source_version_id":version_id,
        "knowledge_release_id":release.as_ref().map(|r| r.knowledge_release_id),
        "page_count":page_count,"completed_pages":success,"failed_pages":failed
    });
    sqlx::query(
        "UPDATE operations SET status=$2,result=$3,error=$4,completed_at=clock_timestamp(),
         updated_at=clock_timestamp() WHERE operation_id=$1",
    )
    .bind(operation_id)
    .bind(if job_status == "failed" {
        "failed"
    } else {
        "succeeded"
    })
    .bind(&result)
    .bind(
        error
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(serialization_error)?,
    )
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    let source_row = sqlx::query(
        "SELECT source_id,revision,kind,name,purpose,state,locator,current_version_id,sync_enabled,
                next_sync_at,last_sync_at
         FROM knowledge_sources WHERE source_id=$1 AND operator_id=$2 AND tenant_id=$3 AND project_id=$4",
    ).bind(source_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid()).fetch_one(&mut *tx).await.map_err(database_error)?;
    let version_row = sqlx::query(
        "SELECT source_version_id,source_id,version,representation,object_id,object_version,content_sha256,captured_at,
                original_url,parent_version_id,parser_version,extraction_version,created_at
         FROM knowledge_source_versions WHERE source_version_id=$1",
    ).bind(version_id).fetch_one(&mut *tx).await.map_err(database_error)?;
    let job_row = sqlx::query(
        "SELECT import_job_id,operation_id,source_id,source_version_id,stage,status,attempt,
                lease_until,input_hash,stage_output_refs,completed_units,failed_units,errors,resumed_from
         FROM knowledge_import_jobs WHERE import_job_id=$1",
    ).bind(lease.job_id).fetch_one(&mut *tx).await.map_err(database_error)?;
    let operation_row =
        sqlx::query("SELECT created_at,updated_at FROM operations WHERE operation_id=$1")
            .bind(operation_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?;
    let original_job: Uuid = sqlx::query_scalar(
        "WITH RECURSIVE ancestors AS (
           SELECT import_job_id,resumed_from FROM knowledge_import_jobs WHERE import_job_id=$1
           UNION ALL
           SELECT parent.import_job_id,parent.resumed_from
             FROM ancestors child JOIN knowledge_import_jobs parent ON parent.import_job_id=child.resumed_from
         ) SELECT import_job_id FROM ancestors WHERE resumed_from IS NULL LIMIT 1",
    ).bind(lease.job_id).fetch_one(&mut *tx).await.map_err(database_error)?;
    let client_item_id: Option<String> = sqlx::query_scalar(
        "SELECT acceptance->>'client_item_id' FROM knowledge_import_receipts
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3
           AND acceptance->'import_job'->>'import_job_id'=$4 LIMIT 1",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(original_job.to_string())
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?;
    let acceptance = ImportAcceptance {
        client_item_id: client_item_id.unwrap_or_else(|| format!("pdf:{}", lease.job_id)),
        status: match job_status {
            "partial" => ImportStatus::Partial,
            "failed" => ImportStatus::Failed,
            _ => ImportStatus::Succeeded,
        },
        source: Some(source_from_row(&source_row, scope, project)?),
        source_version: Some(version_from_row(&version_row, scope, project)?),
        import_job: Some(job_from_row(&job_row, scope, project)?),
        operation: Some(Operation {
            id: operation_id,
            kind: "knowledge.import".to_owned(),
            status: if job_status == "failed" {
                OperationStatus::Failed
            } else {
                OperationStatus::Succeeded
            },
            scope: scope.clone(),
            result: Some(result),
            error: error.clone(),
            created_at: operation_row.get("created_at"),
            updated_at: operation_row.get("updated_at"),
        }),
        release: release.clone(),
        error,
    };
    sqlx::query(
        "INSERT INTO outbox_events
         (event_id,event_type,schema_version,operator_id,tenant_id,project_id,aggregate_id,
          aggregate_version,occurred_at,correlation_id,payload)
         VALUES ($1,$2,1,$3,$4,$5,$6,$7,clock_timestamp(),$8,$9)",
    )
    .bind(Uuid::new_v4())
    .bind(if job_status == "failed" {
        "knowledge.import.failed"
    } else if partial {
        "knowledge.import.partial"
    } else {
        "knowledge.source.version.ready"
    })
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(source_id)
    .bind(task.get::<i32, _>("attempt") as i64)
    .bind(operation_id)
    .bind(
        json!({"source_id":source_id,"source_version_id":version_id,"import_job_id":lease.job_id,
                 "knowledge_release_id":release.as_ref().map(|r| r.knowledge_release_id),
                 "completed_pages":success,"failed_pages":failed}),
    )
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    Ok(acceptance)
}

pub(super) async fn retry(
    repo: &PgKnowledgeRepository,
    scope: &TenantScope,
    job_id: Uuid,
) -> Result<ImportJob, AppError> {
    configured(repo)?;
    let project = PgKnowledgeRepository::project_id(scope)?;
    let mut tx = repo.transaction(scope).await?;
    let old = sqlx::query(
        "SELECT task.source_id,task.source_version_id,task.object_id,task.object_version,
                task.input_sha256,task.parser_profile,task.manifest_schema,task.page_count,
                job.status,job.attempt,source.current_version_id
         FROM knowledge_pdf_parse_tasks task
         JOIN knowledge_import_jobs job ON job.import_job_id=task.import_job_id
         JOIN knowledge_sources source ON source.source_id=task.source_id
         WHERE task.import_job_id=$1 AND task.operator_id=$2 AND task.tenant_id=$3
           AND task.project_id=$4 AND source.state='active'
         FOR UPDATE OF task,job,source",
    )
    .bind(job_id)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::not_found("PDF import job not found"))?;
    if !matches!(
        old.get::<String, _>("status").as_str(),
        "failed" | "partial"
    ) {
        return Err(AppError::conflict(
            "only completed failed or partial PDF jobs can be retried",
        ));
    }
    if repo.pdf_parser_profile.as_deref() != Some(old.get::<String, _>("parser_profile").as_str()) {
        return Err(AppError::conflict(
            "PDF parser profile changed; retry requires frozen profile",
        ));
    }
    let source_id: Uuid = old.get("source_id");
    let prior_version: Uuid = old.get("source_version_id");
    if old.get::<Option<Uuid>, _>("current_version_id") != Some(prior_version) {
        // Only a never-published failed parse with no current version may
        // retry. A later authored/current child must never be overwritten.
        if old.get::<Option<Uuid>, _>("current_version_id").is_some() {
            return Err(AppError::conflict("PDF source has a newer current version"));
        }
        let newer: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM knowledge_import_jobs WHERE source_id=$1 AND resumed_from=$2)",
        ).bind(source_id).bind(job_id).fetch_one(&mut *tx).await.map_err(database_error)?;
        if newer {
            return Err(AppError::conflict("PDF import has a successor job"));
        }
    }
    let successor_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM knowledge_import_jobs
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND resumed_from=$4)",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(job_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    if successor_exists {
        return Err(AppError::conflict("PDF import already has a successor job"));
    }
    let count: i64 = sqlx::query_scalar(
        "SELECT COALESCE(max(version),0) FROM knowledge_source_versions
         WHERE source_id=$1 AND operator_id=$2 AND tenant_id=$3 AND project_id=$4",
    )
    .bind(source_id)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    let version_id = Uuid::new_v4();
    let successor_job = Uuid::new_v4();
    let operation_id = Uuid::new_v4();
    let input_hash: String = old.get("input_sha256");
    let profile: String = old.get("parser_profile");
    let successes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM knowledge_pdf_parse_pages
         WHERE import_job_id=$1 AND status='succeeded'",
    )
    .bind(job_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    sqlx::query(
        "INSERT INTO knowledge_source_versions
         (source_version_id,operator_id,tenant_id,project_id,source_id,version,object_id,object_version,
          content_sha256,captured_at,parent_version_id,parser_version,extraction_version,created_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,clock_timestamp(),$10,$11,'none-v1',clock_timestamp())",
    ).bind(version_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid()).bind(source_id).bind(count+1).bind(old.get::<Uuid,_>("object_id"))
    .bind(old.get::<i64,_>("object_version")).bind(&input_hash).bind(prior_version).bind(&profile)
    .execute(&mut *tx).await.map_err(database_error)?;
    sqlx::query(
        "INSERT INTO operations (operation_id,operator_id,tenant_id,project_id,kind,status)
         VALUES ($1,$2,$3,$4,'knowledge.import','queued')",
    )
    .bind(operation_id)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    sqlx::query(
        "INSERT INTO knowledge_import_jobs
         (import_job_id,operator_id,tenant_id,project_id,operation_id,source_id,source_version_id,
          stage,status,attempt,input_hash,completed_units,resumed_from)
         VALUES ($1,$2,$3,$4,$5,$6,$7,'parse','queued',0,$8,$9,$10)",
    )
    .bind(successor_job)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(operation_id)
    .bind(source_id)
    .bind(version_id)
    .bind(&input_hash)
    .bind(successes as i32)
    .bind(job_id)
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    sqlx::query(
        "INSERT INTO knowledge_pdf_parse_tasks
         (import_job_id,operator_id,tenant_id,project_id,source_id,source_version_id,object_id,
          object_version,input_sha256,parser_profile,manifest_schema,page_count)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(successor_job)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(source_id)
    .bind(version_id)
    .bind(old.get::<Uuid, _>("object_id"))
    .bind(old.get::<i64, _>("object_version"))
    .bind(&input_hash)
    .bind(&profile)
    .bind(old.get::<Option<String>, _>("manifest_schema"))
    .bind(old.get::<Option<i32>, _>("page_count"))
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    sqlx::query(
        "INSERT INTO knowledge_pdf_parse_pages
         (import_job_id,operator_id,tenant_id,project_id,page,status,text,text_sha256)
         SELECT $1,operator_id,tenant_id,project_id,page,status,text,text_sha256
         FROM knowledge_pdf_parse_pages WHERE import_job_id=$2 AND status='succeeded'",
    )
    .bind(successor_job)
    .bind(job_id)
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    sqlx::query(
        "INSERT INTO outbox_events
         (event_id,event_type,schema_version,operator_id,tenant_id,project_id,aggregate_id,
          aggregate_version,occurred_at,correlation_id,payload)
         VALUES ($1,'knowledge.import.accepted',1,$2,$3,$4,$5,$6,clock_timestamp(),$7,$8)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(source_id)
    .bind(count + 1)
    .bind(operation_id)
    .bind(json!({"source_id":source_id,"import_job_id":successor_job,"resumed_from":job_id}))
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    let row = sqlx::query(
        "SELECT import_job_id,operation_id,source_id,source_version_id,stage,status,attempt,
                lease_until,input_hash,stage_output_refs,completed_units,failed_units,errors,resumed_from
         FROM knowledge_import_jobs WHERE import_job_id=$1",
    ).bind(successor_job).fetch_one(&mut *tx).await.map_err(database_error)?;
    let job = job_from_row(&row, scope, project)?;
    tx.commit().await.map_err(database_error)?;
    Ok(job)
}

pub(super) async fn operation(
    repo: &PgKnowledgeRepository,
    scope: &TenantScope,
    job_id: Uuid,
) -> Result<Option<Operation>, AppError> {
    configured(repo)?;
    let project = PgKnowledgeRepository::project_id(scope)?;
    let mut tx = repo.transaction(scope).await?;
    let row = sqlx::query(
        "SELECT op.operation_id,op.kind,op.status,op.result,op.error,op.created_at,op.updated_at
         FROM knowledge_pdf_parse_tasks task
         JOIN knowledge_import_jobs job ON job.import_job_id=task.import_job_id
         JOIN operations op ON op.operation_id=job.operation_id
         WHERE task.import_job_id=$1 AND task.operator_id=$2 AND task.tenant_id=$3 AND task.project_id=$4",
    ).bind(job_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid()).fetch_optional(&mut *tx).await.map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    row.map(|row| {
        Ok(Operation {
            id: row.get("operation_id"),
            kind: row.get("kind"),
            status: match row.get::<String, _>("status").as_str() {
                "queued" => OperationStatus::Queued,
                "running" => OperationStatus::Running,
                "succeeded" => OperationStatus::Succeeded,
                "failed" => OperationStatus::Failed,
                _ => {
                    return Err(AppError::conflict(
                        "PDF operation has invalid persisted status",
                    ));
                }
            },
            scope: scope.clone(),
            result: row.get("result"),
            error: row
                .get::<Option<Value>, _>("error")
                .map(serde_json::from_value)
                .transpose()
                .map_err(serialization_error)?,
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
    })
    .transpose()
}

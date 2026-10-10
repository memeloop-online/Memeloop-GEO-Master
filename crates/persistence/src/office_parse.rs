//! Durable, scoped and fenced DOCX/XLSX structural extraction ledger.
use super::*;
use geo_domain::{
    OFFICE_MAX_DOCUMENT_TEXT_BYTES, OfficeDocumentManifest, OfficeParseCursor, OfficeParseInput,
    OfficeParseJobRef, OfficeParseLease, OfficeUnitResult, office_document_error_code,
    office_unit_chunks,
};

pub(super) const OFFICE_MEDIA_TYPES: [&str; 2] = [
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
];

pub(super) fn is_office_media_type(media: &str) -> bool {
    OFFICE_MEDIA_TYPES.contains(&media)
}

fn configured(repo: &PgKnowledgeRepository) -> Result<(), AppError> {
    if repo.office_parser_profile.is_none() {
        return Err(AppError::capability_missing(
            "Office parser is not configured",
        ));
    }
    Ok(())
}

fn fenced() -> AppError {
    AppError::conflict("Office parse lease expired, fenced, or source is unavailable")
}

fn duration(seconds: i64) -> Result<i32, AppError> {
    i32::try_from(seconds)
        .ok()
        .filter(|seconds| (1..=300).contains(seconds))
        .ok_or_else(|| AppError::invalid_request("invalid Office parse lease duration"))
}

async fn locked_task(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    lease: &OfficeParseLease,
) -> Result<sqlx::postgres::PgRow, AppError> {
    let project = PgKnowledgeRepository::project_id(scope)?;
    // Lock the job before the task everywhere, including claim, so a lease
    // expiring during an output transaction cannot permit an old writer.
    let row = sqlx::query(
        "SELECT import_job_id FROM knowledge_import_jobs
         WHERE import_job_id=$1 AND operator_id=$2 AND tenant_id=$3 AND project_id=$4
           AND status='running' AND lease_until>clock_timestamp() FOR UPDATE",
    )
    .bind(lease.job_id)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    if row.is_none() {
        return Err(fenced());
    }
    sqlx::query(
        "SELECT task.source_id,task.source_version_id,task.object_id,task.object_version,
                task.input_sha256,task.media_type,task.parser_profile,task.manifest_schema,
                task.manifest,task.unit_count,task.released_id,
                job.operation_id,job.attempt,job.resumed_from,source.state
         FROM knowledge_office_parse_tasks task
         JOIN knowledge_import_jobs job ON job.import_job_id=task.import_job_id
         JOIN knowledge_sources source ON source.source_id=task.source_id
         WHERE task.import_job_id=$1 AND task.operator_id=$2 AND task.tenant_id=$3
           AND task.project_id=$4 AND task.lease_id=$5 AND task.fencing_token=$6
           AND job.status='running' AND job.lease_until>clock_timestamp()
           AND source.state='active' FOR UPDATE OF task,source",
    )
    .bind(lease.job_id)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(lease.lease_id)
    .bind(lease.fencing_token)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?
    .ok_or_else(fenced)
}

pub(super) async fn candidates(
    repo: &PgKnowledgeRepository,
    after: Option<OfficeParseCursor>,
    limit: usize,
) -> Result<Vec<OfficeParseJobRef>, AppError> {
    configured(repo)?;
    let rows = sqlx::query(
        "SELECT task.operator_id,task.tenant_id,task.project_id,task.import_job_id,task.created_at
         FROM knowledge_office_parse_tasks task
         JOIN knowledge_import_jobs job ON job.import_job_id=task.import_job_id
         JOIN knowledge_sources source ON source.source_id=task.source_id
         WHERE (job.status='queued' OR (job.status='running' AND job.lease_until<=clock_timestamp()))
           AND source.state='active'
           AND ($1::timestamptz IS NULL OR (task.created_at,task.import_job_id)>($1,$2))
         ORDER BY task.created_at,task.import_job_id LIMIT $3",
    )
    .bind(after.map(|cursor| cursor.created_at))
    .bind(after.map(|cursor| cursor.job_id).unwrap_or(Uuid::nil()))
    .bind(limit.clamp(1, 200) as i64)
    .fetch_all(&repo.pool).await.map_err(database_error)?;
    Ok(rows
        .into_iter()
        .map(|row| OfficeParseJobRef {
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
    lease_seconds: i64,
) -> Result<Option<OfficeParseLease>, AppError> {
    configured(repo)?;
    let seconds = duration(lease_seconds)?;
    let project = PgKnowledgeRepository::project_id(scope)?;
    let mut tx = repo.transaction(scope).await?;
    let row = sqlx::query(
        "UPDATE knowledge_import_jobs job SET status='running',attempt=attempt+1,
            lease_until=clock_timestamp()+make_interval(secs=>$5::int),updated_at=clock_timestamp()
         FROM knowledge_office_parse_tasks task,knowledge_sources source
         WHERE job.import_job_id=$1 AND job.operator_id=$2 AND job.tenant_id=$3 AND job.project_id=$4
           AND task.import_job_id=job.import_job_id AND source.source_id=job.source_id
           AND source.state='active'
           AND (job.status='queued' OR (job.status='running' AND job.lease_until<=clock_timestamp()))
         RETURNING job.lease_until",
    )
    .bind(job_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid()).bind(seconds)
    .fetch_optional(&mut *tx).await.map_err(database_error)?;
    let Some(row) = row else {
        tx.commit().await.map_err(database_error)?;
        return Ok(None);
    };
    let token: i64 = sqlx::query_scalar(
        "UPDATE knowledge_office_parse_tasks SET lease_id=$2,fencing_token=fencing_token+1
         WHERE import_job_id=$1 RETURNING fencing_token",
    )
    .bind(job_id)
    .bind(lease_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    sqlx::query(
        "UPDATE operations SET status='running',updated_at=clock_timestamp()
         WHERE operation_id=(SELECT operation_id FROM knowledge_import_jobs WHERE import_job_id=$1)",
    )
    .bind(job_id).execute(&mut *tx).await.map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    Ok(Some(OfficeParseLease {
        job_id,
        lease_id,
        fencing_token: token,
        expires_at: row.get("lease_until"),
    }))
}

pub(super) async fn renew(
    repo: &PgKnowledgeRepository,
    scope: &TenantScope,
    lease: &OfficeParseLease,
    lease_seconds: i64,
) -> Result<Option<OfficeParseLease>, AppError> {
    configured(repo)?;
    let seconds = duration(lease_seconds)?;
    let mut tx = repo.transaction(scope).await?;
    if locked_task(&mut tx, scope, lease).await.is_err() {
        return Ok(None);
    }
    let expires_at = sqlx::query_scalar::<_, chrono::DateTime<Utc>>(
        "UPDATE knowledge_import_jobs
         SET lease_until=clock_timestamp()+make_interval(secs=>$2::int),
             updated_at=clock_timestamp()
         WHERE import_job_id=$1 RETURNING lease_until",
    )
    .bind(lease.job_id)
    .bind(seconds)
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    Ok(Some(OfficeParseLease {
        expires_at,
        ..lease.clone()
    }))
}

pub(super) async fn input(
    repo: &PgKnowledgeRepository,
    scope: &TenantScope,
    lease: &OfficeParseLease,
) -> Result<OfficeParseInput, AppError> {
    configured(repo)?;
    let mut tx = repo.transaction(scope).await?;
    let task = locked_task(&mut tx, scope, lease).await?;
    let project = PgKnowledgeRepository::project_id(scope)?;
    let object = sqlx::query(
        "SELECT obj.sha256,obj.actual_size,obj.object_version,obj.detected_media_type,
                blob.sha256 AS blob_hash,blob.actual_size AS blob_size,blob.content
         FROM knowledge_stored_objects obj
         JOIN knowledge_upload_sessions session ON session.committed_object_id=obj.object_id
           AND session.operator_id=obj.operator_id AND session.tenant_id=obj.tenant_id
           AND session.project_id=obj.project_id
         JOIN knowledge_upload_blobs blob ON blob.upload_session_id=session.upload_session_id
         WHERE obj.object_id=$1 AND obj.operator_id=$2 AND obj.tenant_id=$3 AND obj.project_id=$4
           AND obj.state='committed' AND session.state='committed'",
    )
    .bind(task.get::<Uuid, _>("object_id"))
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::conflict("Office source object unavailable"))?;
    let bytes: Vec<u8> = object.get("content");
    let hash: String = task.get("input_sha256");
    let size: i64 = object.get("actual_size");
    let media: String = task.get("media_type");
    if object.get::<String, _>("sha256") != hash
        || object.get::<String, _>("blob_hash") != hash
        || object.get::<i64, _>("object_version") != task.get::<i64, _>("object_version")
        || object.get::<i64, _>("blob_size") != size
        || bytes.len() as i64 != size
        || sha256_hex(&bytes) != hash
        || object.get::<String, _>("detected_media_type") != media
        || !is_office_media_type(&media)
    {
        return Err(AppError::conflict(
            "Office source object changed after acceptance",
        ));
    }
    let successful_units = sqlx::query_scalar::<_, i32>(
        "SELECT ordinal FROM knowledge_office_parse_units
         WHERE import_job_id=$1 AND status='succeeded' ORDER BY ordinal",
    )
    .bind(lease.job_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(database_error)?
    .into_iter()
    .map(|ordinal| ordinal as u32)
    .collect();
    let manifest = task
        .get::<Option<Value>, _>("manifest")
        .map(serde_json::from_value)
        .transpose()
        .map_err(serialization_error)?;
    let input = OfficeParseInput {
        bytes,
        input_sha256: hash,
        media_type: media,
        parser_profile: task.get("parser_profile"),
        successful_units,
        manifest,
    };
    tx.commit().await.map_err(database_error)?;
    Ok(input)
}

pub(super) async fn manifest(
    repo: &PgKnowledgeRepository,
    scope: &TenantScope,
    lease: &OfficeParseLease,
    manifest: OfficeDocumentManifest,
) -> Result<(), AppError> {
    configured(repo)?;
    let mut tx = repo.transaction(scope).await?;
    let task = locked_task(&mut tx, scope, lease).await?;
    manifest.validate(
        &task.get::<String, _>("input_sha256"),
        &task.get::<String, _>("parser_profile"),
    )?;
    if manifest.media_type != task.get::<String, _>("media_type") {
        return Err(AppError::invalid_request(
            "Office manifest media type mismatch",
        ));
    }
    let encoded = serde_json::to_value(&manifest).map_err(serialization_error)?;
    if let Some(existing) = task.get::<Option<Value>, _>("manifest") {
        if existing != encoded {
            return Err(AppError::conflict("Office manifest is immutable"));
        }
    } else {
        sqlx::query(
            "UPDATE knowledge_office_parse_tasks
             SET manifest_schema=$2,manifest=$3,unit_count=$4 WHERE import_job_id=$1",
        )
        .bind(lease.job_id)
        .bind(&manifest.schema_version)
        .bind(&encoded)
        .bind(manifest.unit_count() as i32)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        let project = PgKnowledgeRepository::project_id(scope)?;
        for ordinal in 0..manifest.unit_count() {
            sqlx::query(
                "INSERT INTO knowledge_office_parse_units
                 (import_job_id,operator_id,tenant_id,project_id,ordinal)
                 VALUES ($1,$2,$3,$4,$5)",
            )
            .bind(lease.job_id)
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(project.as_uuid())
            .bind(ordinal as i32)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        }
    }
    tx.commit().await.map_err(database_error)?;
    Ok(())
}

pub(super) async fn unit(
    repo: &PgKnowledgeRepository,
    scope: &TenantScope,
    lease: &OfficeParseLease,
    result: OfficeUnitResult,
) -> Result<(), AppError> {
    configured(repo)?;
    let mut tx = repo.transaction(scope).await?;
    let task = locked_task(&mut tx, scope, lease).await?;
    let manifest: OfficeDocumentManifest = serde_json::from_value(
        task.get::<Option<Value>, _>("manifest")
            .ok_or_else(|| AppError::conflict("Office manifest is not recorded"))?,
    )
    .map_err(serialization_error)?;
    manifest.validate(
        &task.get::<String, _>("input_sha256"),
        &task.get::<String, _>("parser_profile"),
    )?;
    result.validate(&manifest)?;
    let ordinal = result.unit_id() as i32;
    let mut result = result;
    if result.is_success()
        && office_unit_chunks(scope, task.get("source_version_id"), &manifest, &result, 0)?
            .is_empty()
    {
        result = OfficeUnitResult::Failure {
            unit_id: ordinal as u32,
            code: "empty_text".to_owned(),
        };
    }
    let encoded = serde_json::to_value(&result).map_err(serialization_error)?;
    let hash = sha256_hex(&serde_json::to_vec(&result).map_err(serialization_error)?);
    let existing = sqlx::query(
        "SELECT status,result_sha256 FROM knowledge_office_parse_units
         WHERE import_job_id=$1 AND ordinal=$2 FOR UPDATE",
    )
    .bind(lease.job_id)
    .bind(ordinal)
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::conflict("Office manifest unit is missing"))?;
    let status: String = existing.get("status");
    if status == "succeeded" {
        if !result.is_success() || existing.get::<Option<String>, _>("result_sha256") != Some(hash)
        {
            return Err(AppError::conflict("successful Office unit is immutable"));
        }
        return Ok(());
    }
    // A failed unit may be replaced during the same live attempt; a successful
    // output is immutable even across leases, and retries copy it verbatim.
    let bytes: i64 = sqlx::query_scalar(
        "SELECT COALESCE(sum(octet_length(result::text)),0)::bigint
         FROM knowledge_office_parse_units WHERE import_job_id=$1 AND status='succeeded'",
    )
    .bind(lease.job_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    let result = if result.is_success()
        && bytes
            + serde_json::to_vec(&result)
                .map_err(serialization_error)?
                .len() as i64
            > OFFICE_MAX_DOCUMENT_TEXT_BYTES as i64
    {
        OfficeUnitResult::Failure {
            unit_id: ordinal as u32,
            code: "unit_limit".to_owned(),
        }
    } else {
        result
    };
    let encoded = if result.is_success() {
        encoded
    } else {
        serde_json::to_value(&result).map_err(serialization_error)?
    };
    let (state, code) = match &result {
        OfficeUnitResult::Failure { code, .. } => ("failed", Some(code.clone())),
        _ => ("succeeded", None),
    };
    let hash = sha256_hex(&serde_json::to_vec(&result).map_err(serialization_error)?);
    sqlx::query(
        "UPDATE knowledge_office_parse_units
         SET status=$3,result=$4,result_sha256=$5,error_code=$6,updated_at=clock_timestamp()
         WHERE import_job_id=$1 AND ordinal=$2",
    )
    .bind(lease.job_id)
    .bind(ordinal)
    .bind(state)
    .bind(encoded)
    .bind(hash)
    .bind(code)
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    let counts = sqlx::query(
        "SELECT count(*) FILTER (WHERE status='succeeded')::integer AS success,
                count(*) FILTER (WHERE status='failed')::integer AS failed
         FROM knowledge_office_parse_units WHERE import_job_id=$1",
    )
    .bind(lease.job_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    let error_rows = sqlx::query(
        "SELECT ordinal,error_code FROM knowledge_office_parse_units
         WHERE import_job_id=$1 AND status='failed' ORDER BY ordinal LIMIT 100",
    )
    .bind(lease.job_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(database_error)?;
    let format = if task.get::<String, _>("media_type") == OFFICE_MEDIA_TYPES[0] {
        "docx"
    } else {
        "xlsx"
    };
    let errors: Vec<Value> = error_rows.into_iter().map(|r|
        json!({"unit_id":r.get::<i32,_>("ordinal"),"format":format,"code":r.get::<String,_>("error_code")})
    ).collect();
    sqlx::query(
        "UPDATE knowledge_import_jobs SET completed_units=$2,failed_units=$3,errors=$4,
           updated_at=clock_timestamp() WHERE import_job_id=$1",
    )
    .bind(lease.job_id)
    .bind(counts.get::<i32, _>("success"))
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
    lease: &OfficeParseLease,
    terminal_error: Option<&str>,
) -> Result<ImportAcceptance, AppError> {
    configured(repo)?;
    if terminal_error.is_some_and(|code| !office_document_error_code(code)) {
        return Err(AppError::invalid_request(
            "invalid Office document failure code",
        ));
    }
    let project = PgKnowledgeRepository::project_id(scope)?;
    let mut tx = repo.transaction(scope).await?;
    let task = locked_task(&mut tx, scope, lease).await?;
    // Parsing can take longer than the object read at claim time. Re-check
    // committed bytes and immutable source-version identity at publication,
    // and hold shared locks through the release transaction.
    let original = sqlx::query(
        "SELECT obj.sha256,obj.actual_size,obj.object_version,obj.detected_media_type,
                blob.sha256 AS blob_hash,blob.actual_size AS blob_size,blob.content,
                version.content_sha256 AS version_hash,version.parser_version,
                version.object_id AS version_object_id,
                version.object_version AS version_object_version,
                job.input_hash AS job_hash
         FROM knowledge_stored_objects obj
         JOIN knowledge_upload_sessions session ON session.committed_object_id=obj.object_id
           AND session.operator_id=obj.operator_id AND session.tenant_id=obj.tenant_id
           AND session.project_id=obj.project_id
         JOIN knowledge_upload_blobs blob ON blob.upload_session_id=session.upload_session_id
         JOIN knowledge_source_versions version ON version.source_version_id=$5
           AND version.operator_id=obj.operator_id AND version.tenant_id=obj.tenant_id
           AND version.project_id=obj.project_id
         JOIN knowledge_import_jobs job ON job.import_job_id=$6
           AND job.operator_id=obj.operator_id AND job.tenant_id=obj.tenant_id
           AND job.project_id=obj.project_id
         WHERE obj.object_id=$1 AND obj.operator_id=$2 AND obj.tenant_id=$3 AND obj.project_id=$4
           AND obj.state='committed' AND session.state='committed'
         FOR SHARE OF obj,session,blob,version",
    )
    .bind(task.get::<Uuid, _>("object_id"))
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(task.get::<Uuid, _>("source_version_id"))
    .bind(lease.job_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::conflict("Office source object is unavailable"))?;
    let bytes: Vec<u8> = original.get("content");
    let hash: String = task.get("input_sha256");
    let size: i64 = original.get("actual_size");
    let media: String = task.get("media_type");
    let profile: String = task.get("parser_profile");
    let object_id: Uuid = task.get("object_id");
    let object_version: i64 = task.get("object_version");
    if original.get::<String, _>("sha256") != hash
        || original.get::<String, _>("blob_hash") != hash
        || original.get::<String, _>("version_hash") != hash
        || original.get::<String, _>("job_hash") != hash
        || original.get::<Uuid, _>("version_object_id") != object_id
        || original.get::<i64, _>("version_object_version") != object_version
        || original.get::<i64, _>("object_version") != object_version
        || original.get::<i64, _>("blob_size") != size
        || bytes.len() as i64 != size
        || sha256_hex(&bytes) != hash
        || original.get::<String, _>("detected_media_type") != media
        || original.get::<String, _>("parser_version") != profile
    {
        return Err(AppError::conflict(
            "Office source object changed before release",
        ));
    }
    let manifest: Option<OfficeDocumentManifest> = task
        .get::<Option<Value>, _>("manifest")
        .map(serde_json::from_value)
        .transpose()
        .map_err(serialization_error)?;
    if terminal_error.is_none() && manifest.is_none() {
        return Err(AppError::conflict("Office manifest is not recorded"));
    }
    if terminal_error.is_some() && manifest.is_some() {
        return Err(AppError::conflict(
            "Office unit manifest already recorded; use unit failures",
        ));
    }
    if let Some(manifest) = &manifest {
        manifest.validate(
            &task.get::<String, _>("input_sha256"),
            &task.get::<String, _>("parser_profile"),
        )?;
        if manifest.media_type != task.get::<String, _>("media_type") {
            return Err(AppError::conflict("Office manifest media type changed"));
        }
    }
    let counts = sqlx::query(
        "SELECT count(*) FILTER (WHERE status='succeeded')::integer AS success,
                count(*) FILTER (WHERE status='failed')::integer AS failed,
                count(*) FILTER (WHERE status='pending')::integer AS pending
         FROM knowledge_office_parse_units WHERE import_job_id=$1",
    )
    .bind(lease.job_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    let success: i32 = counts.get("success");
    let failed: i32 = counts.get("failed");
    let pending: i32 = counts.get("pending");
    if terminal_error.is_none() && pending != 0 {
        return Err(AppError::conflict("Office units do not cover the manifest"));
    }
    if let Some(manifest) = &manifest {
        let total = success + failed + pending;
        if total != manifest.unit_count() as i32
            || task.get::<Option<i32>, _>("unit_count") != Some(total)
        {
            return Err(AppError::conflict(
                "Office unit ledger does not match manifest",
            ));
        }
    }
    let failures = failed + pending + i32::from(terminal_error.is_some());
    let status = if success == 0 || terminal_error.is_some() {
        "failed"
    } else if failures != 0 {
        "partial"
    } else {
        "succeeded"
    };
    let version_id: Uuid = task.get("source_version_id");
    let source_id: Uuid = task.get("source_id");
    if status != "failed" {
        let manifest = manifest
            .as_ref()
            .expect("successful units imply frozen manifest");
        let mut cursor: i32 = -1;
        let mut chunk_ordinal: i32 = 0;
        // Keyset reads are bounded per round; never scan unrelated jobs or
        // read full historical extraction JSON into one application buffer.
        loop {
            let rows = sqlx::query(
                "SELECT ordinal,result FROM knowledge_office_parse_units
                 WHERE import_job_id=$1 AND ordinal>$2 AND status='succeeded'
                 ORDER BY ordinal LIMIT 128",
            )
            .bind(lease.job_id)
            .bind(cursor)
            .fetch_all(&mut *tx)
            .await
            .map_err(database_error)?;
            if rows.is_empty() {
                break;
            }
            for row in rows {
                cursor = row.get("ordinal");
                let result: OfficeUnitResult = serde_json::from_value(row.get("result"))
                    .map_err(|_| AppError::conflict("Office result is corrupt"))?;
                if result.unit_id() != cursor as u32 || !result.is_success() {
                    return Err(AppError::conflict("Office result does not match unit"));
                }
                for chunk in
                    office_unit_chunks(scope, version_id, manifest, &result, chunk_ordinal)?
                {
                    sqlx::query(
                        "INSERT INTO knowledge_chunks
                         (chunk_id,operator_id,tenant_id,project_id,source_version_id,ordinal,kind,
                          text,text_hash,locator,product_ids,market,language,extraction_method,confidence)
                         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,'[]'::jsonb,NULL,NULL,$11,$12)",
                    )
                    .bind(chunk.chunk_id).bind(scope.operator_id.as_uuid())
                    .bind(scope.tenant_id.as_uuid()).bind(project.as_uuid()).bind(version_id)
                    .bind(chunk.ordinal)
                    .bind(match chunk.kind {
                        geo_domain::ChunkKind::Paragraph => "paragraph",
                        geo_domain::ChunkKind::Table => "table",
                        geo_domain::ChunkKind::ImageDescription => "image_description",
                    })
                    .bind(&chunk.text).bind(&chunk.text_hash)
                    .bind(serde_json::to_value(&chunk.locator).map_err(serialization_error)?)
                    .bind(&chunk.extraction_method).bind(chunk.confidence)
                    .execute(&mut *tx).await.map_err(database_error)?;
                    chunk_ordinal += 1;
                }
            }
        }
        // A syntactically successful unit with no substantive text is not
        // searchable evidence; it must not switch a release pointer.
        if chunk_ordinal == 0 {
            return Err(AppError::conflict(
                "Office output contains no searchable evidence",
            ));
        }
    }
    let mut errors = Vec::new();
    let error_rows = sqlx::query(
        "SELECT ordinal,error_code FROM knowledge_office_parse_units
         WHERE import_job_id=$1 AND status='failed' ORDER BY ordinal LIMIT 100",
    )
    .bind(lease.job_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(database_error)?;
    let format = if task.get::<String, _>("media_type") == OFFICE_MEDIA_TYPES[0] {
        "docx"
    } else {
        "xlsx"
    };
    for row in error_rows {
        errors.push(json!({"unit_id":row.get::<i32,_>("ordinal"),"format":format,"code":row.get::<String,_>("error_code")}));
    }
    if let Some(code) = terminal_error {
        errors.push(json!({"code":code}));
    }
    sqlx::query(
        "UPDATE knowledge_import_jobs
         SET status=$2,stage=$3,completed_units=$4,failed_units=$5,errors=$6,
             lease_until=NULL,updated_at=clock_timestamp() WHERE import_job_id=$1",
    )
    .bind(lease.job_id)
    .bind(status)
    .bind(if status == "failed" {
        "parse"
    } else {
        "release"
    })
    .bind(success)
    .bind(failures)
    .bind(json!(errors))
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    if status != "failed" {
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
    let release = if status == "failed" {
        None
    } else {
        Some(PgKnowledgeRepository::create_release(&mut tx, scope).await?)
    };
    if let Some(release) = &release {
        sqlx::query(
            "UPDATE knowledge_office_parse_tasks SET released_id=$2 WHERE import_job_id=$1",
        )
        .bind(lease.job_id)
        .bind(release.knowledge_release_id)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    }
    let operation_id: Uuid = task.get("operation_id");
    let error = terminal_error
        .map(|code| AppError::invalid_request(format!("Office parse failed: {code}")))
        .or_else(|| {
            (success == 0).then(|| AppError::invalid_request("no Office units were extracted"))
        });
    let result = json!({
        "source_id":source_id,"source_version_id":version_id,
        "knowledge_release_id":release.as_ref().map(|r|r.knowledge_release_id),
        "unit_count":manifest.as_ref().map(OfficeDocumentManifest::unit_count),
        "completed_units":success,"failed_units":failures,
    });
    sqlx::query(
        "UPDATE operations SET status=$2,result=$3,error=$4,completed_at=clock_timestamp(),
         updated_at=clock_timestamp() WHERE operation_id=$1",
    )
    .bind(operation_id)
    .bind(if status == "failed" {
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
                next_sync_at,last_sync_at FROM knowledge_sources
         WHERE source_id=$1 AND operator_id=$2 AND tenant_id=$3 AND project_id=$4",
    )
    .bind(source_id)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    let version_row = sqlx::query(
        "SELECT source_version_id,source_id,version,representation,object_id,object_version,content_sha256,captured_at,
                original_url,parent_version_id,parser_version,extraction_version,created_at
         FROM knowledge_source_versions WHERE source_version_id=$1",
    )
    .bind(version_id).fetch_one(&mut *tx).await.map_err(database_error)?;
    let job_row = sqlx::query(
        "SELECT import_job_id,operation_id,source_id,source_version_id,stage,status,attempt,
                lease_until,input_hash,stage_output_refs,completed_units,failed_units,errors,resumed_from
         FROM knowledge_import_jobs WHERE import_job_id=$1",
    )
    .bind(lease.job_id).fetch_one(&mut *tx).await.map_err(database_error)?;
    let operation_row =
        sqlx::query("SELECT created_at,updated_at FROM operations WHERE operation_id=$1")
            .bind(operation_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?;
    let initial_job: Uuid = sqlx::query_scalar(
        "WITH RECURSIVE ancestors AS (
           SELECT import_job_id,resumed_from FROM knowledge_import_jobs WHERE import_job_id=$1
           UNION ALL
           SELECT parent.import_job_id,parent.resumed_from
           FROM ancestors child JOIN knowledge_import_jobs parent ON parent.import_job_id=child.resumed_from
         ) SELECT import_job_id FROM ancestors WHERE resumed_from IS NULL LIMIT 1",
    )
    .bind(lease.job_id).fetch_one(&mut *tx).await.map_err(database_error)?;
    let client_item_id: Option<String> = sqlx::query_scalar(
        "SELECT acceptance->>'client_item_id' FROM knowledge_import_receipts
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3
           AND acceptance->'import_job'->>'import_job_id'=$4 LIMIT 1",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(initial_job.to_string())
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?;
    let acceptance = ImportAcceptance {
        client_item_id: client_item_id.unwrap_or_else(|| format!("office:{}", lease.job_id)),
        status: match status {
            "failed" => ImportStatus::Failed,
            "partial" => ImportStatus::Partial,
            _ => ImportStatus::Succeeded,
        },
        source: Some(source_from_row(&source_row, scope, project)?),
        source_version: Some(version_from_row(&version_row, scope, project)?),
        import_job: Some(job_from_row(&job_row, scope, project)?),
        operation: Some(Operation {
            id: operation_id,
            kind: "knowledge.import".to_owned(),
            status: if status == "failed" {
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
    .bind(if status == "failed" {
        "knowledge.import.failed"
    } else if status == "partial" {
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
         "knowledge_release_id":release.as_ref().map(|r|r.knowledge_release_id),
         "completed_units":success,"failed_units":failures}),
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
                task.input_sha256,task.media_type,task.parser_profile,task.manifest,
                task.manifest_schema,task.unit_count,job.status,source.current_version_id
         FROM knowledge_office_parse_tasks task
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
    .ok_or_else(|| AppError::not_found("Office import job not found"))?;
    if !matches!(
        old.get::<String, _>("status").as_str(),
        "failed" | "partial"
    ) {
        return Err(AppError::conflict(
            "only completed failed or partial Office jobs can be retried",
        ));
    }
    let successor = sqlx::query(
        "SELECT job.import_job_id,job.operation_id,job.source_id,job.source_version_id,
                job.stage,job.status,job.attempt,job.lease_until,job.input_hash,
                job.stage_output_refs,job.completed_units,job.failed_units,job.errors,job.resumed_from
         FROM knowledge_import_jobs job JOIN knowledge_office_parse_tasks task
           ON task.import_job_id=job.import_job_id
         WHERE job.operator_id=$1 AND job.tenant_id=$2 AND job.project_id=$3
           AND job.resumed_from=$4 AND task.source_id=$5
         ORDER BY job.created_at,job.import_job_id LIMIT 1",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(job_id)
    .bind(old.get::<Uuid, _>("source_id"))
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?;
    if let Some(row) = successor {
        let existing = job_from_row(&row, scope, project)?;
        tx.commit().await.map_err(database_error)?;
        return Ok(existing);
    }
    if repo.office_parser_profile.as_deref()
        != Some(old.get::<String, _>("parser_profile").as_str())
    {
        return Err(AppError::conflict(
            "Office parser profile changed; retry requires frozen profile",
        ));
    }
    let source_id: Uuid = old.get("source_id");
    let prior_version: Uuid = old.get("source_version_id");
    let current: Option<Uuid> = old.get("current_version_id");
    if current != Some(prior_version) {
        // A failed attempt never releases, so its parent can still be the
        // active version. An unrelated replacement is not safe to retry.
        let previous: Option<Uuid> = sqlx::query_scalar(
            "SELECT parent_version_id FROM knowledge_source_versions
             WHERE source_version_id=$1 AND operator_id=$2 AND tenant_id=$3 AND project_id=$4",
        )
        .bind(prior_version)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
        if current != previous {
            return Err(AppError::conflict("Office source has since been replaced"));
        }
    }
    let next_version: i64 = sqlx::query_scalar(
        "SELECT COALESCE(max(version),0)+1 FROM knowledge_source_versions
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
    let new_job = Uuid::new_v4();
    let operation_id = Uuid::new_v4();
    let hash: String = old.get("input_sha256");
    let profile: String = old.get("parser_profile");
    let successes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM knowledge_office_parse_units
         WHERE import_job_id=$1 AND status='succeeded'",
    )
    .bind(job_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    sqlx::query(
        "INSERT INTO knowledge_source_versions
         (source_version_id,operator_id,tenant_id,project_id,source_id,version,object_id,
          object_version,content_sha256,captured_at,parent_version_id,parser_version,extraction_version,created_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,clock_timestamp(),$10,$11,'none-v1',clock_timestamp())",
    )
    .bind(version_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid()).bind(source_id).bind(next_version)
    .bind(old.get::<Uuid,_>("object_id")).bind(old.get::<i64,_>("object_version"))
    .bind(&hash).bind(prior_version).bind(&profile)
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
         (import_job_id,operator_id,tenant_id,project_id,operation_id,source_id,
          source_version_id,stage,status,attempt,input_hash,completed_units,resumed_from)
         VALUES ($1,$2,$3,$4,$5,$6,$7,'parse','queued',0,$8,$9,$10)",
    )
    .bind(new_job)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(operation_id)
    .bind(source_id)
    .bind(version_id)
    .bind(&hash)
    .bind(successes as i32)
    .bind(job_id)
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    sqlx::query(
        "INSERT INTO knowledge_office_parse_tasks
         (import_job_id,operator_id,tenant_id,project_id,source_id,source_version_id,
          object_id,object_version,input_sha256,media_type,parser_profile,
          manifest_schema,manifest,unit_count)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
    )
    .bind(new_job)
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(project.as_uuid())
    .bind(source_id)
    .bind(version_id)
    .bind(old.get::<Uuid, _>("object_id"))
    .bind(old.get::<i64, _>("object_version"))
    .bind(&hash)
    .bind(old.get::<String, _>("media_type"))
    .bind(&profile)
    .bind(old.get::<Option<String>, _>("manifest_schema"))
    .bind(old.get::<Option<Value>, _>("manifest"))
    .bind(old.get::<Option<i32>, _>("unit_count"))
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    // The prior manifest and every successful unit are immutable evidence.
    // Pending/failed units get clean slots; only they are dispatched again.
    sqlx::query(
        "INSERT INTO knowledge_office_parse_units
         (import_job_id,operator_id,tenant_id,project_id,ordinal,status,result,result_sha256,error_code)
         SELECT $1,operator_id,tenant_id,project_id,ordinal,
                CASE WHEN status='succeeded' THEN 'succeeded' ELSE 'pending' END,
                CASE WHEN status='succeeded' THEN result ELSE NULL END,
                CASE WHEN status='succeeded' THEN result_sha256 ELSE NULL END,
                NULL
         FROM knowledge_office_parse_units WHERE import_job_id=$2 ORDER BY ordinal",
    )
    .bind(new_job).bind(job_id).execute(&mut *tx).await.map_err(database_error)?;
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
    .bind(next_version)
    .bind(operation_id)
    .bind(json!({"source_id":source_id,"import_job_id":new_job,"resumed_from":job_id}))
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    let row = sqlx::query(
        "SELECT import_job_id,operation_id,source_id,source_version_id,stage,status,attempt,
                lease_until,input_hash,stage_output_refs,completed_units,failed_units,errors,resumed_from
         FROM knowledge_import_jobs WHERE import_job_id=$1",
    )
    .bind(new_job).fetch_one(&mut *tx).await.map_err(database_error)?;
    let job = job_from_row(&row, scope, project)?;
    tx.commit().await.map_err(database_error)?;
    Ok(job)
}

pub(super) async fn operation(
    repo: &PgKnowledgeRepository,
    scope: &TenantScope,
    job_id: Uuid,
) -> Result<Option<Operation>, AppError> {
    // The generic retry dispatcher probes each parser ledger, including
    // deployments where this optional adapter has never been configured.
    let project = PgKnowledgeRepository::project_id(scope)?;
    let mut tx = repo.transaction(scope).await?;
    let row = sqlx::query(
        "SELECT op.operation_id,op.kind,op.status,op.result,op.error,op.created_at,op.updated_at
         FROM knowledge_office_parse_tasks task
         JOIN knowledge_import_jobs job ON job.import_job_id=task.import_job_id
         JOIN operations op ON op.operation_id=job.operation_id
         WHERE task.import_job_id=$1 AND task.operator_id=$2 AND task.tenant_id=$3 AND task.project_id=$4",
    )
    .bind(job_id).bind(scope.operator_id.as_uuid()).bind(scope.tenant_id.as_uuid())
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
                        "Office operation has invalid persisted status",
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

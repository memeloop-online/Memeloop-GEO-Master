//! Durable, project-scoped media-use grants. Callers of the transaction helper
//! must already have locked the project row (and any content rows) FOR UPDATE.
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use geo_domain::{
    AppError, ContentMediaBinding, ContentMediaBindingState, ContentMediaRepository,
    MAX_UPLOAD_BYTES, MediaObjectKey, TenantScope, VerifiedImage, sha256_hex,
};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::{Database, set_local_scope};

#[derive(Clone)]
pub struct PgContentMediaRepository {
    pool: PgPool,
}

impl PgContentMediaRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn from_database(database: &Database) -> Self {
        Self::new(database.pool().clone())
    }

    async fn transaction(
        &self,
        scope: &TenantScope,
    ) -> Result<Transaction<'_, Postgres>, AppError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        set_local_scope(&mut tx, scope).await.map_err(db)?;
        Ok(tx)
    }
}

fn project(scope: &TenantScope) -> Result<Uuid, AppError> {
    scope
        .project_id
        .map(|id| id.as_uuid())
        .ok_or_else(|| AppError::invalid_request("project scope required"))
}

async fn lock_project(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
) -> Result<(), AppError> {
    let id = project(scope)?;
    let found: Option<Uuid> = sqlx::query_scalar(
        "SELECT project_id FROM projects
         WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 FOR UPDATE",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db)?;
    if found.is_none() {
        return Err(AppError::not_found("media project not found"));
    }
    Ok(())
}

fn binding_from_row(row: &PgRow, scope: &TenantScope) -> Result<ContentMediaBinding, AppError> {
    let state: String = row.get("state");
    let state = match state.as_str() {
        "active" => ContentMediaBindingState::Active,
        "withdrawn" => ContentMediaBindingState::Withdrawn,
        _ => return Err(AppError::conflict("invalid media binding state")),
    };
    Ok(ContentMediaBinding {
        binding_id: row.get("binding_id"),
        operator_id: scope.operator_id,
        tenant_id: scope.tenant_id,
        project_id: scope
            .project_id
            .ok_or_else(|| AppError::invalid_request("project scope required"))?,
        image: VerifiedImage {
            key: MediaObjectKey {
                object_id: row.get("object_id"),
                object_version: row.get("object_version"),
                sha256: row.get("sha256"),
            },
            media_type: row.get("media_type"),
            byte_len: u64::try_from(row.get::<i64, _>("byte_len"))
                .map_err(|_| AppError::conflict("invalid media byte length"))?,
            width: u32::try_from(row.get::<i32, _>("width"))
                .map_err(|_| AppError::conflict("invalid media width"))?,
            height: u32::try_from(row.get::<i32, _>("height"))
                .map_err(|_| AppError::conflict("invalid media height"))?,
        },
        state,
        created_at: row.get::<DateTime<Utc>, _>("created_at"),
        withdrawn_at: row.get("withdrawn_at"),
    })
}

const BINDING_COLUMNS: &str = "binding_id,object_id,object_version,sha256,media_type,byte_len,width,height,state,created_at,withdrawn_at";

/// Lock each object in identity order, then each grant in identity order, then
/// read and verify the committed bytes. The caller owns the transaction and
/// already holds the project row lock. Never call this in a separate transaction
/// from the content write/reuse/ready decision.
pub(crate) async fn validate_content_media_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    keys: &[MediaObjectKey],
) -> Result<Vec<ContentMediaBinding>, AppError> {
    let project_id = project(scope)?;
    let mut ordered = keys.to_vec();
    ordered.sort_by(|a, b| {
        (&a.object_id, a.object_version, &a.sha256).cmp(&(
            &b.object_id,
            b.object_version,
            &b.sha256,
        ))
    });
    ordered.dedup_by(|a, b| {
        a.object_id == b.object_id && a.object_version == b.object_version && a.sha256 == b.sha256
    });
    // Every object lock precedes every grant lock. In particular a later key
    // cannot invert the lock order against a concurrent multi-image writer.
    for key in &ordered {
        let found: Option<Uuid> = sqlx::query_scalar(
            "SELECT object_id FROM knowledge_stored_objects
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND object_id=$4
               AND object_version=$5 AND sha256=$6 AND state='committed'
             FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id)
        .bind(key.object_id)
        .bind(key.object_version)
        .bind(&key.sha256)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db)?;
        if found.is_none() {
            return Err(AppError::conflict("media object is unavailable"));
        }
    }
    let mut bindings = Vec::with_capacity(ordered.len());
    for key in &ordered {
        let row = sqlx::query(&format!(
            "SELECT {BINDING_COLUMNS} FROM content_media_bindings
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3
               AND object_id=$4 AND object_version=$5 AND sha256=$6 FOR UPDATE"
        ))
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(project_id)
        .bind(key.object_id)
        .bind(key.object_version)
        .bind(&key.sha256)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::conflict("media binding is unavailable"))?;
        let binding = binding_from_row(&row, scope)?;
        if binding.state != ContentMediaBindingState::Active {
            return Err(AppError::conflict("media binding has been withdrawn"));
        }
        bindings.push(binding);
    }
    for binding in &bindings {
        validate_bytes(tx, scope, &binding.image).await?;
    }
    Ok(bindings)
}

// Never trust the stored object's upload-declared media type as a decoder
// result. The VerifiedImage's type and dimensions come from the image decoder
// in the domain adapter; this gate separately verifies immutable byte identity.
async fn validate_bytes(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantScope,
    image: &VerifiedImage,
) -> Result<(), AppError> {
    let id = project(scope)?;
    let row = sqlx::query(
        "SELECT object.backend,object.opaque_key,object.actual_size,
                session.upload_session_id,session.expected_size,session.expected_sha256,
                blob.actual_size AS blob_size,blob.sha256 AS blob_hash,
                CASE WHEN octet_length(blob.content) <= $7 THEN blob.content END AS content
         FROM knowledge_stored_objects object
         JOIN knowledge_upload_sessions session
           ON session.committed_object_id=object.object_id
          AND session.operator_id=object.operator_id AND session.tenant_id=object.tenant_id
          AND session.project_id=object.project_id
         JOIN knowledge_upload_blobs blob ON blob.upload_session_id=session.upload_session_id
         WHERE object.operator_id=$1 AND object.tenant_id=$2 AND object.project_id=$3
           AND object.object_id=$4 AND object.object_version=$5 AND object.sha256=$6
           AND object.state='committed' AND session.state='committed'
           AND session.staging_object_ref='agent-attachment'
         FOR SHARE OF session, blob",
    )
    .bind(scope.operator_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(id)
    .bind(image.key.object_id)
    .bind(image.key.object_version)
    .bind(&image.key.sha256)
    .bind(MAX_UPLOAD_BYTES as i64)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db)?
    .ok_or_else(|| AppError::conflict("committed media attachment is unavailable"))?;
    let session_id: Uuid = row.get("upload_session_id");
    let bytes: Option<Vec<u8>> = row.get("content");
    let size = i64::try_from(image.byte_len)
        .map_err(|_| AppError::conflict("invalid media byte length"))?;
    if row.get::<String, _>("backend") != "postgres_blob"
        || row.get::<String, _>("opaque_key") != format!("upload/{session_id}")
        || row.get::<i64, _>("actual_size") != size
        || row.get::<i64, _>("expected_size") != size
        || row.get::<String, _>("expected_sha256") != image.key.sha256
        || row.get::<Option<i64>, _>("blob_size") != Some(size)
        || row.get::<Option<String>, _>("blob_hash").as_deref() != Some(image.key.sha256.as_str())
        || bytes.as_deref().is_none_or(|content| {
            content.len() as i64 != size || sha256_hex(content) != image.key.sha256
        })
    {
        return Err(AppError::conflict(
            "committed media attachment bytes changed",
        ));
    }
    // The blob row was locked before its bytes were read and remains locked
    // through the caller's commit.
    Ok(())
}

#[async_trait]
impl ContentMediaRepository for PgContentMediaRepository {
    async fn create_binding(
        &self,
        scope: &TenantScope,
        image: VerifiedImage,
    ) -> Result<ContentMediaBinding, AppError> {
        image.validate()?;
        let id = project(scope)?;
        let mut tx = self.transaction(scope).await?;
        lock_project(&mut tx, scope).await?;
        // Serialize on the object before inspecting a possibly withdrawn grant.
        let found: Option<Uuid> = sqlx::query_scalar(
            "SELECT object_id FROM knowledge_stored_objects
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND object_id=$4
               AND object_version=$5 AND sha256=$6 AND state='committed' FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(id)
        .bind(image.key.object_id)
        .bind(image.key.object_version)
        .bind(&image.key.sha256)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        if found.is_none() {
            return Err(AppError::conflict("media object is unavailable"));
        }
        let existing = sqlx::query(&format!(
            "SELECT {BINDING_COLUMNS} FROM content_media_bindings
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3
               AND object_id=$4 AND object_version=$5 AND sha256=$6 FOR UPDATE"
        ))
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(id)
        .bind(image.key.object_id)
        .bind(image.key.object_version)
        .bind(&image.key.sha256)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        if let Some(row) = &existing {
            let binding = binding_from_row(row, scope)?;
            if binding.state != ContentMediaBindingState::Active {
                return Err(AppError::conflict("media binding has been withdrawn"));
            }
            if binding.image != image {
                return Err(AppError::conflict(
                    "media image metadata differs from existing binding",
                ));
            }
        }
        validate_bytes(&mut tx, scope, &image).await?;
        let result = if let Some(row) = &existing {
            binding_from_row(row, scope)?
        } else {
            let row = sqlx::query(&format!(
                "INSERT INTO content_media_bindings
                 (binding_id,operator_id,tenant_id,project_id,object_id,object_version,sha256,
                  media_type,byte_len,width,height)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
                 RETURNING {BINDING_COLUMNS}"
            ))
            .bind(Uuid::new_v4())
            .bind(scope.operator_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(id)
            .bind(image.key.object_id)
            .bind(image.key.object_version)
            .bind(&image.key.sha256)
            .bind(&image.media_type)
            .bind(
                i64::try_from(image.byte_len)
                    .map_err(|_| AppError::invalid_request("invalid image size"))?,
            )
            .bind(
                i32::try_from(image.width)
                    .map_err(|_| AppError::invalid_request("invalid image width"))?,
            )
            .bind(
                i32::try_from(image.height)
                    .map_err(|_| AppError::invalid_request("invalid image height"))?,
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
            binding_from_row(&row, scope)?
        };
        tx.commit().await.map_err(db)?;
        Ok(result)
    }

    async fn get_binding(
        &self,
        scope: &TenantScope,
        binding_id: Uuid,
    ) -> Result<Option<ContentMediaBinding>, AppError> {
        let id = project(scope)?;
        let mut tx = self.transaction(scope).await?;
        let row = sqlx::query(&format!(
            "SELECT {BINDING_COLUMNS} FROM content_media_bindings
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND binding_id=$4"
        ))
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(id)
        .bind(binding_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        row.as_ref()
            .map(|row| binding_from_row(row, scope))
            .transpose()
    }

    async fn list_bindings(
        &self,
        scope: &TenantScope,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ContentMediaBinding>, AppError> {
        // The API requests one extra row to determine whether a 100-item page
        // has a next cursor; callers cannot request an unbounded read.
        if limit == 0 || limit > 101 {
            return Err(AppError::invalid_request(
                "media page size must be between 1 and 101",
            ));
        }
        let id = project(scope)?;
        let mut tx = self.transaction(scope).await?;
        lock_project(&mut tx, scope).await?;
        let rows = sqlx::query(&format!(
            "SELECT {BINDING_COLUMNS} FROM content_media_bindings
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3
               AND state='active' AND ($4::uuid IS NULL OR binding_id > $4)
             ORDER BY binding_id LIMIT $5"
        ))
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(id)
        .bind(after)
        .bind(limit as i64)
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?;
        let mut bindings = Vec::with_capacity(rows.len());
        for row in &rows {
            bindings.push(binding_from_row(row, scope)?);
        }
        // The page is bound to the same live bytes as a content write.
        let verified = validate_content_media_in_transaction(
            &mut tx,
            scope,
            &bindings
                .iter()
                .map(|binding| binding.image.key.clone())
                .collect::<Vec<_>>(),
        )
        .await?;
        let by_id: std::collections::HashMap<_, _> = verified
            .into_iter()
            .map(|binding| (binding.binding_id, binding))
            .collect();
        let result = bindings
            .into_iter()
            .map(|binding| {
                by_id
                    .get(&binding.binding_id)
                    .cloned()
                    .ok_or_else(|| AppError::conflict("media page changed"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().await.map_err(db)?;
        Ok(result)
    }

    async fn withdraw_binding(
        &self,
        scope: &TenantScope,
        binding_id: Uuid,
    ) -> Result<ContentMediaBinding, AppError> {
        let id = project(scope)?;
        let mut tx = self.transaction(scope).await?;
        lock_project(&mut tx, scope).await?;
        // Lock object first, then grant; the project lock prevents a competing
        // grant withdrawal from changing identity between these statements.
        let key = sqlx::query(
            "SELECT object_id,object_version,sha256 FROM content_media_bindings
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND binding_id=$4",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(id)
        .bind(binding_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::not_found("media binding not found"))?;
        sqlx::query(
            "SELECT object_id FROM knowledge_stored_objects
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND object_id=$4 FOR UPDATE",
        )
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(id)
        .bind(key.get::<Uuid, _>("object_id"))
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or_else(|| AppError::conflict("media object is unavailable"))?;
        let row = sqlx::query(&format!(
            "SELECT {BINDING_COLUMNS} FROM content_media_bindings
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND binding_id=$4 FOR UPDATE"
        ))
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(id)
        .bind(binding_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        let previous = binding_from_row(&row, scope)?;
        if previous.state == ContentMediaBindingState::Withdrawn {
            tx.commit().await.map_err(db)?;
            return Ok(previous);
        }
        let row = sqlx::query(&format!(
            "UPDATE content_media_bindings SET state='withdrawn',withdrawn_at=now()
             WHERE operator_id=$1 AND tenant_id=$2 AND project_id=$3 AND binding_id=$4
             RETURNING {BINDING_COLUMNS}"
        ))
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(id)
        .bind(binding_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        let result = binding_from_row(&row, scope)?;
        tx.commit().await.map_err(db)?;
        Ok(result)
    }
}

fn db(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        geo_domain::ErrorCode::DependencyUnavailable,
        format!("database error: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DatabaseConfig;
    use std::time::Duration;

    #[tokio::test]
    #[ignore = "requires a disposable PostgreSQL database in GEO_TEST_DATABASE_URL"]
    async fn content_validation_holds_object_and_binding_until_caller_commits() {
        let config = DatabaseConfig::from_url(
            std::env::var("GEO_TEST_DATABASE_URL").expect("disposable database URL"),
        )
        .expect("database config");
        let database = Database::connect_and_migrate(&config)
            .await
            .expect("migrations");
        let pool = database.pool();
        let op = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let project = Uuid::new_v4();
        let session = Uuid::new_v4();
        let object = Uuid::new_v4();
        let data = b"synthetic locked object";
        let hash = sha256_hex(data);
        sqlx::query(
            "INSERT INTO operators (operator_id,slug,display_name) VALUES ($1,$2,'Media lock')",
        )
        .bind(op)
        .bind(format!("lock-{op}"))
        .execute(pool)
        .await
        .expect("operator");
        sqlx::query("INSERT INTO tenants (tenant_id,operator_id,slug,display_name) VALUES ($1,$2,$3,'Media lock')")
            .bind(tenant).bind(op).bind(format!("lock-{tenant}")).execute(pool).await.expect("tenant");
        sqlx::query("INSERT INTO projects (project_id,operator_id,tenant_id,slug,display_name) VALUES ($1,$2,$3,$4,'Media lock')")
            .bind(project).bind(op).bind(tenant).bind(format!("lock-{project}"))
            .execute(pool).await.expect("project");
        sqlx::query(
            "INSERT INTO knowledge_upload_sessions
             (upload_session_id,operator_id,tenant_id,project_id,revision,filename,
              declared_media_type,expected_size,expected_sha256,purpose,state,expires_at,
              committed_object_id,staging_object_ref)
             VALUES ($1,$2,$3,$4,2,'lock.png','image/png',$5,$6,'internal','committed',
                     now()+interval '1 day',$7,'agent-attachment')",
        )
        .bind(session)
        .bind(op)
        .bind(tenant)
        .bind(project)
        .bind(data.len() as i64)
        .bind(&hash)
        .bind(object)
        .execute(pool)
        .await
        .expect("upload");
        sqlx::query("INSERT INTO knowledge_upload_blobs (upload_session_id,content,actual_size,sha256) VALUES ($1,$2,$3,$4)")
            .bind(session).bind(data.as_slice()).bind(data.len() as i64).bind(&hash)
            .execute(pool).await.expect("blob");
        sqlx::query(
            "INSERT INTO knowledge_stored_objects
             (object_id,operator_id,tenant_id,project_id,object_version,backend,opaque_key,
              actual_size,detected_media_type,sha256,state)
             VALUES ($1,$2,$3,$4,1,'postgres_blob',$5,$6,'image/png',$7,'committed')",
        )
        .bind(object)
        .bind(op)
        .bind(tenant)
        .bind(project)
        .bind(format!("upload/{session}"))
        .bind(data.len() as i64)
        .bind(&hash)
        .execute(pool)
        .await
        .expect("object");
        let scope = TenantScope::new(op.into(), tenant.into(), Some(project.into()));
        let key = MediaObjectKey {
            object_id: object,
            object_version: 1,
            sha256: hash,
        };
        let repo = PgContentMediaRepository::from_database(&database);
        let binding = repo
            .create_binding(
                &scope,
                VerifiedImage {
                    key: key.clone(),
                    media_type: "image/png".into(),
                    byte_len: data.len() as u64,
                    width: 1,
                    height: 1,
                },
            )
            .await
            .expect("grant");
        let mut tx = repo.transaction(&scope).await.expect("writer transaction");
        lock_project(&mut tx, &scope).await.expect("project lock");
        assert_eq!(
            validate_content_media_in_transaction(&mut tx, &scope, std::slice::from_ref(&key))
                .await
                .expect("same-transaction validation")[0]
                .binding_id,
            binding.binding_id,
        );
        let contender = PgContentMediaRepository::from_database(&database);
        let contender_scope = scope.clone();
        let attempt = tokio::spawn(async move {
            contender
                .withdraw_binding(&contender_scope, binding.binding_id)
                .await
        });
        let mut attempt = attempt;
        assert!(
            tokio::time::timeout(Duration::from_millis(150), &mut attempt)
                .await
                .is_err(),
            "withdrawal must wait for the content writer's transaction",
        );
        tx.commit().await.expect("writer commit");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), attempt)
                .await
                .expect("withdrawal released")
                .expect("task")
                .expect("withdraw")
                .state,
            ContentMediaBindingState::Withdrawn,
        );
        let mut second_tx = repo.transaction(&scope).await.expect("second transaction");
        lock_project(&mut second_tx, &scope)
            .await
            .expect("project lock");
        assert_eq!(
            validate_content_media_in_transaction(&mut second_tx, &scope, &[key])
                .await
                .expect_err("revocation is terminal")
                .code,
            geo_domain::ErrorCode::Conflict,
        );
    }
}

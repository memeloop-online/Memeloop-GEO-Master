use async_trait::async_trait;
use chrono::Duration;
use geo_domain::{
    AppError, AuthRepository, LoginIdentity, Membership, Operator, OperatorId, Role, Session,
    SessionCredentials, SessionId, User, UserId, hash_token, normalize_email, normalize_host,
};
use sqlx::{PgPool, Row, postgres::PgRow};
use uuid::Uuid;

/// PostgreSQL authentication repository. It never creates a development
/// identity: an empty or unavailable production database is an authentication
/// failure, not a reason to fall back to memory state.
#[derive(Clone)]
pub struct PgAuthRepository {
    pool: PgPool,
}

impl PgAuthRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub fn from_database(database: &crate::Database) -> Self {
        Self::new(database.pool().clone())
    }
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

#[async_trait]
impl AuthRepository for PgAuthRepository {
    async fn operator_for_host(&self, host: &str) -> Result<Option<Operator>, AppError> {
        let row = sqlx::query(
            r#"SELECT o.operator_id, o.slug, o.display_name, o.created_at, o.updated_at
               FROM operator_hosts h JOIN operators o ON o.operator_id = h.operator_id
               WHERE h.host = $1"#,
        )
        .bind(normalize_host(host))
        .fetch_optional(&self.pool)
        .await
        .map_err(database_unavailable)?;
        row.map(operator_from_row).transpose()
    }

    async fn authenticate(
        &self,
        operator_id: OperatorId,
        email: &str,
        password: &str,
    ) -> Result<Option<LoginIdentity>, AppError> {
        let normalized_email = normalize_email(email)?;
        let row = sqlx::query(
            r#"SELECT user_id, operator_id, email, display_name, password_hash, active,
                      created_at, updated_at
               FROM users WHERE operator_id = $1 AND email = $2 AND active
                 AND EXISTS (
                     SELECT 1 FROM memberships
                     WHERE memberships.user_id = users.user_id
                       AND memberships.operator_id = users.operator_id
                       AND memberships.active
                 )"#,
        )
        .bind(operator_id.as_uuid())
        .bind(&normalized_email)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_unavailable)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let user = user_from_row(&row)?;
        let valid = tokio::task::spawn_blocking({
            let user = user.clone();
            let password = password.to_owned();
            move || user.verify_password(&password)
        })
        .await
        .map_err(|_| {
            AppError::new(
                geo_domain::ErrorCode::Internal,
                "password verification failed",
            )
        })?;
        if !valid {
            return Ok(None);
        }
        let memberships = self.memberships(user.id, operator_id).await?;
        if memberships.is_empty() {
            return Ok(None);
        }
        let operator = sqlx::query(
            r#"SELECT operator_id, slug, display_name, created_at, updated_at
               FROM operators WHERE operator_id = $1"#,
        )
        .bind(operator_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(database_unavailable)?
        .ok_or_else(|| AppError::unauthorized("operator is not configured"))?;
        Ok(Some(LoginIdentity {
            operator: operator_from_row(operator)?,
            user,
            memberships,
        }))
    }

    async fn find_session(
        &self,
        operator_id: OperatorId,
        token: &str,
    ) -> Result<Option<Session>, AppError> {
        let row = sqlx::query(
            r#"SELECT session_id, operator_id, user_id, csrf_token, token_hash,
                      created_at, expires_at, revoked_at
               FROM sessions WHERE operator_id = $1 AND token_hash = $2"#,
        )
        .bind(operator_id.as_uuid())
        .bind(hash_token(token))
        .fetch_optional(&self.pool)
        .await
        .map_err(database_unavailable)?;
        row.map(session_from_row).transpose()
    }

    async fn find_user(
        &self,
        operator_id: OperatorId,
        user_id: UserId,
    ) -> Result<Option<User>, AppError> {
        let row = sqlx::query(
            r#"SELECT user_id, operator_id, email, display_name, password_hash, active,
                      created_at, updated_at
               FROM users WHERE operator_id = $1 AND user_id = $2"#,
        )
        .bind(operator_id.as_uuid())
        .bind(user_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(database_unavailable)?;
        row.map(|value| user_from_row(&value)).transpose()
    }

    async fn memberships(
        &self,
        user_id: UserId,
        operator_id: OperatorId,
    ) -> Result<Vec<Membership>, AppError> {
        let rows = sqlx::query(
            r#"SELECT m.membership_id, m.user_id, m.operator_id, m.tenant_id,
                      t.slug AS tenant_slug, t.display_name AS tenant_display_name,
                      m.role, m.active, m.created_at, m.updated_at
               FROM memberships m
               JOIN tenants t ON t.operator_id = m.operator_id AND t.tenant_id = m.tenant_id
               WHERE m.user_id = $1 AND m.operator_id = $2 AND m.active
               ORDER BY created_at ASC, membership_id ASC"#,
        )
        .bind(user_id.as_uuid())
        .bind(operator_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(database_unavailable)?;
        rows.into_iter().map(membership_from_row).collect()
    }

    async fn create_session(
        &self,
        operator_id: OperatorId,
        user_id: UserId,
        ttl: Duration,
    ) -> Result<SessionCredentials, AppError> {
        let credentials = Session::new(operator_id, user_id, ttl);
        let result = sqlx::query(
            r#"INSERT INTO sessions
                (session_id, operator_id, user_id, csrf_token, token_hash,
                 created_at, expires_at, revoked_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#,
        )
        .bind(credentials.session.id.as_uuid())
        .bind(credentials.session.operator_id.as_uuid())
        .bind(credentials.session.user_id.as_uuid())
        .bind(credentials.session.csrf_token())
        .bind(credentials.session.token_hash())
        .bind(credentials.session.created_at)
        .bind(credentials.session.expires_at)
        .bind(credentials.session.revoked_at)
        .execute(&self.pool)
        .await
        .map_err(database_unavailable)?;
        if result.rows_affected() != 1 {
            return Err(AppError::new(
                geo_domain::ErrorCode::Internal,
                "session was not created",
            ));
        }
        Ok(credentials)
    }

    async fn revoke_session(
        &self,
        operator_id: OperatorId,
        session_id: SessionId,
    ) -> Result<(), AppError> {
        sqlx::query("UPDATE sessions SET revoked_at = COALESCE(revoked_at, now()), last_seen_at = now() WHERE operator_id = $1 AND session_id = $2")
            .bind(operator_id.as_uuid())
            .bind(session_id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(database_unavailable)?;
        Ok(())
    }
}

fn operator_from_row(row: PgRow) -> Result<Operator, AppError> {
    Ok(Operator {
        id: row
            .try_get::<Uuid, _>("operator_id")
            .map_err(database_unavailable)?
            .into(),
        slug: row.try_get("slug").map_err(database_unavailable)?,
        display_name: row.try_get("display_name").map_err(database_unavailable)?,
        created_at: row.try_get("created_at").map_err(database_unavailable)?,
        updated_at: row.try_get("updated_at").map_err(database_unavailable)?,
    })
}

fn user_from_row(row: &PgRow) -> Result<User, AppError> {
    User::from_password_hash(
        row.try_get::<Uuid, _>("user_id")
            .map_err(database_unavailable)?
            .into(),
        row.try_get::<Uuid, _>("operator_id")
            .map_err(database_unavailable)?
            .into(),
        row.try_get::<String, _>("email")
            .map_err(database_unavailable)?,
        row.try_get::<String, _>("display_name")
            .map_err(database_unavailable)?,
        row.try_get("active").map_err(database_unavailable)?,
        row.try_get::<String, _>("password_hash")
            .map_err(database_unavailable)?,
        row.try_get("created_at").map_err(database_unavailable)?,
        row.try_get("updated_at").map_err(database_unavailable)?,
    )
}

fn membership_from_row(row: PgRow) -> Result<Membership, AppError> {
    Ok(Membership {
        id: row.try_get("membership_id").map_err(database_unavailable)?,
        user_id: row
            .try_get::<Uuid, _>("user_id")
            .map_err(database_unavailable)?
            .into(),
        operator_id: row
            .try_get::<Uuid, _>("operator_id")
            .map_err(database_unavailable)?
            .into(),
        tenant_id: row
            .try_get::<Uuid, _>("tenant_id")
            .map_err(database_unavailable)?
            .into(),
        tenant_slug: row.try_get("tenant_slug").map_err(database_unavailable)?,
        tenant_display_name: row
            .try_get("tenant_display_name")
            .map_err(database_unavailable)?,
        role: Role::parse(
            &row.try_get::<String, _>("role")
                .map_err(database_unavailable)?,
        )?,
        active: row.try_get("active").map_err(database_unavailable)?,
        created_at: row.try_get("created_at").map_err(database_unavailable)?,
        updated_at: row.try_get("updated_at").map_err(database_unavailable)?,
    })
}

fn session_from_row(row: PgRow) -> Result<Session, AppError> {
    Ok(Session::with_tokens(
        row.try_get::<Uuid, _>("session_id")
            .map_err(database_unavailable)?
            .into(),
        row.try_get::<Uuid, _>("operator_id")
            .map_err(database_unavailable)?
            .into(),
        row.try_get::<Uuid, _>("user_id")
            .map_err(database_unavailable)?
            .into(),
        row.try_get::<String, _>("csrf_token")
            .map_err(database_unavailable)?,
        row.try_get::<String, _>("token_hash")
            .map_err(database_unavailable)?,
        row.try_get("created_at").map_err(database_unavailable)?,
        row.try_get("expires_at").map_err(database_unavailable)?,
        row.try_get("revoked_at").map_err(database_unavailable)?,
    ))
}

fn database_unavailable(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        geo_domain::ErrorCode::DependencyUnavailable,
        format!("authentication persistence is unavailable: {error}"),
    )
}

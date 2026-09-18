//! Authentication and authorization contracts.
//!
//! The HTTP layer only receives opaque session tokens. Password verification,
//! membership lookup, and session persistence live behind this trait so the
//! development memory adapter and PostgreSQL adapter enforce the same
//! server-side tenant boundary.

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tokio::sync::RwLock;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{AppError, Operator, OperatorId, TenantId};

pub const DEVELOPMENT_USER_EMAIL: &str = "demo@localhost";
pub const DEFAULT_SESSION_TTL_SECS: i64 = 60 * 60 * 12;

macro_rules! auth_id {
    ($name:ident) => {
        #[derive(
            Debug,
            Clone,
            Copy,
            PartialEq,
            Eq,
            Hash,
            PartialOrd,
            Ord,
            Serialize,
            Deserialize,
            ToSchema,
        )]
        #[schema(value_type = String)]
        pub struct $name(pub Uuid);

        impl $name {
            pub const fn new(value: Uuid) -> Self {
                Self(value)
            }
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self(value)
            }
        }
        impl From<$name> for Uuid {
            fn from(value: $name) -> Self {
                value.0
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

auth_id!(UserId);
auth_id!(SessionId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    CustomerAdmin,
    CustomerMember,
    CustomerReadOnly,
    Operator,
    ResourceAdmin,
    OemAdmin,
}

impl Role {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CustomerAdmin => "customer_admin",
            Self::CustomerMember => "customer_member",
            Self::CustomerReadOnly => "customer_read_only",
            Self::Operator => "operator",
            Self::ResourceAdmin => "resource_admin",
            Self::OemAdmin => "oem_admin",
        }
    }

    pub fn parse(value: &str) -> Result<Self, AppError> {
        match value {
            "customer_admin" => Ok(Self::CustomerAdmin),
            "customer_member" => Ok(Self::CustomerMember),
            "customer_read_only" => Ok(Self::CustomerReadOnly),
            "operator" => Ok(Self::Operator),
            "resource_admin" => Ok(Self::ResourceAdmin),
            "oem_admin" => Ok(Self::OemAdmin),
            _ => Err(AppError::new(
                crate::ErrorCode::Internal,
                format!("invalid role in database: {value}"),
            )),
        }
    }

    pub const fn can_write(self) -> bool {
        !matches!(self, Self::CustomerReadOnly)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct User {
    pub id: UserId,
    pub operator_id: OperatorId,
    #[serde(rename = "login_name")]
    pub email: String,
    pub display_name: String,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing, skip_deserializing)]
    password_hash: String,
}

impl User {
    pub fn new(
        id: UserId,
        operator_id: OperatorId,
        email: impl Into<String>,
        display_name: impl Into<String>,
        password: &str,
    ) -> Result<Self, AppError> {
        let email = normalize_email(&email.into())?;
        let display_name = validate_text("display_name", display_name.into(), 200)?;
        if password.is_empty() {
            return Err(AppError::invalid_request("password must not be empty"));
        }
        let now = Utc::now();
        Ok(Self {
            id,
            operator_id,
            email,
            display_name,
            active: true,
            created_at: now,
            updated_at: now,
            password_hash: hash_password(password)?,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_password_hash(
        id: UserId,
        operator_id: OperatorId,
        email: impl Into<String>,
        display_name: impl Into<String>,
        active: bool,
        password_hash: impl Into<String>,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    ) -> Result<Self, AppError> {
        Ok(Self {
            id,
            operator_id,
            email: normalize_email(&email.into())?,
            display_name: validate_text("display_name", display_name.into(), 200)?,
            active,
            created_at,
            updated_at,
            password_hash: password_hash.into(),
        })
    }

    pub fn password_hash(&self) -> &str {
        &self.password_hash
    }

    pub fn verify_password(&self, password: &str) -> bool {
        let Ok(parsed) = PasswordHash::new(&self.password_hash) else {
            return false;
        };
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Membership {
    pub id: Uuid,
    pub user_id: UserId,
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    pub tenant_slug: String,
    pub tenant_display_name: String,
    pub role: Role,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Membership {
    pub fn new(user_id: UserId, operator_id: OperatorId, tenant_id: TenantId, role: Role) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            user_id,
            operator_id,
            tenant_id,
            tenant_slug: String::new(),
            tenant_display_name: String::new(),
            role,
            active: true,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Session {
    pub id: SessionId,
    pub operator_id: OperatorId,
    pub user_id: UserId,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing)]
    csrf_token: String,
    #[serde(skip_serializing)]
    token_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCredentials {
    pub session: Session,
    pub token: String,
}

impl Session {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(operator_id: OperatorId, user_id: UserId, ttl: Duration) -> SessionCredentials {
        let now = Utc::now();
        let token = random_token();
        let session = Self {
            id: SessionId::from(Uuid::new_v4()),
            operator_id,
            user_id,
            created_at: now,
            expires_at: now + ttl,
            revoked_at: None,
            csrf_token: random_token(),
            token_hash: hash_token(&token),
        };
        SessionCredentials { session, token }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_tokens(
        id: SessionId,
        operator_id: OperatorId,
        user_id: UserId,
        csrf_token: impl Into<String>,
        token_hash: impl Into<String>,
        created_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
        revoked_at: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            id,
            operator_id,
            user_id,
            created_at,
            expires_at,
            revoked_at,
            csrf_token: csrf_token.into(),
            token_hash: token_hash.into(),
        }
    }

    pub fn csrf_token(&self) -> &str {
        &self.csrf_token
    }
    pub fn token_hash(&self) -> &str {
        &self.token_hash
    }
    pub fn is_active_at(&self, now: DateTime<Utc>) -> bool {
        self.revoked_at.is_none() && self.expires_at > now
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginIdentity {
    pub operator: Operator,
    pub user: User,
    pub memberships: Vec<Membership>,
}

#[async_trait]
pub trait AuthRepository: Send + Sync {
    async fn operator_for_host(&self, host: &str) -> Result<Option<Operator>, AppError>;
    async fn authenticate(
        &self,
        operator_id: OperatorId,
        email: &str,
        password: &str,
    ) -> Result<Option<LoginIdentity>, AppError>;
    async fn find_session(
        &self,
        operator_id: OperatorId,
        token: &str,
    ) -> Result<Option<Session>, AppError>;
    async fn find_user(
        &self,
        operator_id: OperatorId,
        user_id: UserId,
    ) -> Result<Option<User>, AppError>;
    async fn memberships(
        &self,
        user_id: UserId,
        operator_id: OperatorId,
    ) -> Result<Vec<Membership>, AppError>;
    async fn create_session(
        &self,
        operator_id: OperatorId,
        user_id: UserId,
        ttl: Duration,
    ) -> Result<SessionCredentials, AppError>;
    async fn revoke_session(
        &self,
        operator_id: OperatorId,
        session_id: SessionId,
    ) -> Result<(), AppError>;
}

#[derive(Debug, Default)]
struct MemoryAuthData {
    operators: HashMap<OperatorId, Operator>,
    host_operators: HashMap<String, OperatorId>,
    users: HashMap<UserId, User>,
    memberships: Vec<Membership>,
    sessions: HashMap<SessionId, Session>,
}

/// Explicitly non-durable authentication adapter for loopback development.
#[derive(Debug, Default)]
pub struct MemoryAuthRepository {
    data: RwLock<MemoryAuthData>,
}

impl MemoryAuthRepository {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn development_with_password(password: &str) -> Self {
        let operator = Operator::new(crate::DEVELOPMENT_OPERATOR_ID, "memeloop", "模因循环")
            .expect("development operator is valid");
        let user = User::new(
            UserId::from(Uuid::from_u128(0x00000000000040008000000000000004)),
            operator.id,
            DEVELOPMENT_USER_EMAIL,
            "Local Demo",
            password,
        )
        .expect("development user is valid");
        let mut membership = Membership::new(
            user.id,
            operator.id,
            crate::DEVELOPMENT_TENANT_ID,
            Role::CustomerAdmin,
        );
        membership.tenant_slug = "demo".to_owned();
        membership.tenant_display_name = "Local Demo Tenant".to_owned();
        let mut data = MemoryAuthData::default();
        for host in [
            "localhost",
            "localhost:5173",
            "localhost:8080",
            "127.0.0.1",
            "127.0.0.1:5173",
            "127.0.0.1:8080",
        ] {
            data.host_operators.insert(host.to_owned(), operator.id);
        }
        data.operators.insert(operator.id, operator);
        data.users.insert(user.id, user);
        data.memberships.push(membership);
        Self {
            data: RwLock::new(data),
        }
    }

    pub async fn insert_operator(
        &self,
        operator: Operator,
        hosts: &[String],
    ) -> Result<(), AppError> {
        let mut data = self.data.write().await;
        if data.operators.contains_key(&operator.id) {
            return Err(AppError::conflict("operator already exists"));
        }
        for host in hosts {
            let host = normalize_host(host);
            if host.is_empty() || data.host_operators.contains_key(&host) {
                return Err(AppError::conflict("operator host already exists"));
            }
            data.host_operators.insert(host, operator.id);
        }
        data.operators.insert(operator.id, operator);
        Ok(())
    }

    pub async fn insert_user(&self, user: User) -> Result<(), AppError> {
        let mut data = self.data.write().await;
        if data.users.values().any(|candidate| {
            candidate.operator_id == user.operator_id && candidate.email == user.email
        }) {
            return Err(AppError::conflict("user email already exists for operator"));
        }
        if data.users.insert(user.id, user).is_some() {
            return Err(AppError::conflict("user already exists"));
        }
        Ok(())
    }

    pub async fn insert_membership(&self, membership: Membership) -> Result<(), AppError> {
        let mut data = self.data.write().await;
        if !data.users.contains_key(&membership.user_id)
            || !data.operators.contains_key(&membership.operator_id)
        {
            return Err(AppError::invalid_request(
                "membership references an unknown identity",
            ));
        }
        if data.memberships.iter().any(|candidate| {
            candidate.user_id == membership.user_id
                && candidate.operator_id == membership.operator_id
                && candidate.tenant_id == membership.tenant_id
        }) {
            return Err(AppError::conflict("membership already exists"));
        }
        data.memberships.push(membership);
        Ok(())
    }
}

#[async_trait]
impl AuthRepository for MemoryAuthRepository {
    async fn operator_for_host(&self, host: &str) -> Result<Option<Operator>, AppError> {
        let data = self.data.read().await;
        let Some(operator_id) = data.host_operators.get(&normalize_host(host)) else {
            return Ok(None);
        };
        Ok(data.operators.get(operator_id).cloned())
    }

    async fn authenticate(
        &self,
        operator_id: OperatorId,
        email: &str,
        password: &str,
    ) -> Result<Option<LoginIdentity>, AppError> {
        let (operator, user, memberships) = {
            let data = self.data.read().await;
            let normalized = normalize_email(email)?;
            let Some(user) = data
                .users
                .values()
                .find(|user| {
                    user.operator_id == operator_id && user.email == normalized && user.active
                })
                .cloned()
            else {
                return Ok(None);
            };
            let Some(operator) = data.operators.get(&operator_id).cloned() else {
                return Ok(None);
            };
            let memberships = data
                .memberships
                .iter()
                .filter(|membership| {
                    membership.user_id == user.id
                        && membership.operator_id == operator_id
                        && membership.active
                })
                .cloned()
                .collect::<Vec<_>>();
            (operator, user, memberships)
        };
        let password = password.to_owned();
        let password_user = user.clone();
        if memberships.is_empty()
            || !tokio::task::spawn_blocking(move || password_user.verify_password(&password))
                .await
                .map_err(|_| {
                    AppError::new(crate::ErrorCode::Internal, "password verification failed")
                })?
        {
            return Ok(None);
        }
        Ok(Some(LoginIdentity {
            operator,
            user,
            memberships,
        }))
    }

    async fn find_session(
        &self,
        operator_id: OperatorId,
        token: &str,
    ) -> Result<Option<Session>, AppError> {
        let token_hash = hash_token(token);
        Ok(self
            .data
            .read()
            .await
            .sessions
            .values()
            .find(|session| {
                session.operator_id == operator_id && session.token_hash() == token_hash
            })
            .cloned())
    }

    async fn find_user(
        &self,
        operator_id: OperatorId,
        user_id: UserId,
    ) -> Result<Option<User>, AppError> {
        Ok(self
            .data
            .read()
            .await
            .users
            .get(&user_id)
            .filter(|user| user.operator_id == operator_id)
            .cloned())
    }

    async fn memberships(
        &self,
        user_id: UserId,
        operator_id: OperatorId,
    ) -> Result<Vec<Membership>, AppError> {
        Ok(self
            .data
            .read()
            .await
            .memberships
            .iter()
            .filter(|membership| {
                membership.user_id == user_id
                    && membership.operator_id == operator_id
                    && membership.active
            })
            .cloned()
            .collect())
    }

    async fn create_session(
        &self,
        operator_id: OperatorId,
        user_id: UserId,
        ttl: Duration,
    ) -> Result<SessionCredentials, AppError> {
        let mut data = self.data.write().await;
        if !data
            .users
            .get(&user_id)
            .is_some_and(|user| user.operator_id == operator_id && user.active)
        {
            return Err(AppError::unauthorized("user does not exist"));
        }
        let credentials = Session::new(operator_id, user_id, ttl);
        data.sessions
            .insert(credentials.session.id, credentials.session.clone());
        Ok(credentials)
    }

    async fn revoke_session(
        &self,
        operator_id: OperatorId,
        session_id: SessionId,
    ) -> Result<(), AppError> {
        let mut data = self.data.write().await;
        if let Some(session) = data.sessions.get_mut(&session_id)
            && session.operator_id == operator_id
        {
            session.revoked_at = Some(Utc::now());
        }
        Ok(())
    }
}

pub fn normalize_host(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

pub fn normalize_email(email: &str) -> Result<String, AppError> {
    let email = email.trim().to_ascii_lowercase();
    if email.is_empty() || email.len() > 320 || !email.contains('@') {
        return Err(AppError::invalid_request("email must be a valid address"));
    }
    Ok(email)
}

fn validate_text(field: &str, value: String, max: usize) -> Result<String, AppError> {
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Err(AppError::invalid_request(format!(
            "{field} must not be empty"
        )));
    }
    if value.chars().count() > max {
        return Err(AppError::invalid_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(value)
}

fn hash_password(password: &str) -> Result<String, AppError> {
    let salt = argon2::password_hash::SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| AppError::new(crate::ErrorCode::Internal, "password hashing failed"))
}

fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

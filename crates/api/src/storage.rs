use async_trait::async_trait;
use geo_domain::{AppError, EventEnvelope, Operation, TenantScope};
use geo_persistence::Database;
use sqlx::{PgPool, Row, postgres::PgRow};
use std::collections::HashMap;
use tokio::sync::{RwLock, broadcast};
use uuid::Uuid;

#[async_trait]
pub trait OperationStore: Send + Sync {
    async fn get(&self, scope: &TenantScope, id: Uuid) -> Result<Option<Operation>, AppError>;

    /// Persist a newly created operation. Implementations that cannot accept
    /// writes must fail closed instead of silently dropping a start request.
    async fn save(&self, _operation: Operation) -> Result<(), AppError> {
        Err(AppError::new(
            geo_domain::ErrorCode::DependencyUnavailable,
            "operation persistence does not support writes",
        ))
    }
}

/// Development-only in-memory operation store. It is not durable and must not
/// be presented as PostgreSQL persistence.
#[derive(Debug, Default)]
pub struct MemoryOperationStore {
    operations: RwLock<HashMap<Uuid, Operation>>,
}

impl MemoryOperationStore {
    pub async fn insert(&self, operation: Operation) {
        self.operations
            .write()
            .await
            .insert(operation.id, operation);
    }

    pub async fn clear(&self) {
        self.operations.write().await.clear();
    }

    pub async fn len(&self) -> usize {
        self.operations.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.operations.read().await.is_empty()
    }
}

#[async_trait]
impl OperationStore for MemoryOperationStore {
    async fn get(&self, scope: &TenantScope, id: Uuid) -> Result<Option<Operation>, AppError> {
        Ok(self
            .operations
            .read()
            .await
            .get(&id)
            .filter(|operation| scope.contains(&operation.scope))
            .cloned())
    }

    async fn save(&self, operation: Operation) -> Result<(), AppError> {
        self.insert(operation).await;
        Ok(())
    }
}

#[derive(Clone)]
pub struct PgOperationStore {
    pool: PgPool,
}

impl PgOperationStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn from_database(database: &Database) -> Self {
        Self::new(database.pool().clone())
    }
}

#[async_trait]
impl OperationStore for PgOperationStore {
    async fn get(&self, scope: &TenantScope, id: Uuid) -> Result<Option<Operation>, AppError> {
        let row = sqlx::query(
            r#"SELECT operation_id, operator_id, tenant_id, project_id, kind, status,
                      result, error, created_at, updated_at
               FROM operations
               WHERE operation_id = $1
                 AND operator_id = $2
                 AND tenant_id = $3
                 AND ($4::uuid IS NULL OR project_id = $4)"#,
        )
        .bind(id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(scope.project_id.map(|project_id| project_id.as_uuid()))
        .fetch_optional(&self.pool)
        .await
        .map_err(database_unavailable)?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(operation_from_row(row)?))
    }

    async fn save(&self, operation: Operation) -> Result<(), AppError> {
        let scope = operation.scope.clone();
        let mut transaction = self.pool.begin().await.map_err(database_unavailable)?;
        geo_persistence::set_local_scope(&mut transaction, &scope)
            .await
            .map_err(database_unavailable)?;
        let status = match operation.status {
            geo_domain::OperationStatus::Queued => "queued",
            geo_domain::OperationStatus::Running => "running",
            geo_domain::OperationStatus::Succeeded => "succeeded",
            geo_domain::OperationStatus::Failed => "failed",
        };
        sqlx::query(
            r#"INSERT INTO operations
                (operation_id, operator_id, tenant_id, project_id, kind, status,
                 result, error, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
               ON CONFLICT (operation_id) DO NOTHING"#,
        )
        .bind(operation.id)
        .bind(scope.operator_id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(scope.project_id.map(|project_id| project_id.as_uuid()))
        .bind(&operation.kind)
        .bind(status)
        .bind(operation.result)
        .bind(
            operation
                .error
                .map(|error| serde_json::to_value(error).map_err(serialization_error))
                .transpose()?,
        )
        .bind(operation.created_at)
        .bind(operation.updated_at)
        .execute(&mut *transaction)
        .await
        .map_err(database_unavailable)?;
        transaction.commit().await.map_err(database_unavailable)
    }
}

fn operation_from_row(row: PgRow) -> Result<Operation, AppError> {
    let status = match row
        .try_get::<String, _>("status")
        .map_err(database_unavailable)?
        .as_str()
    {
        "queued" => geo_domain::OperationStatus::Queued,
        "running" => geo_domain::OperationStatus::Running,
        "succeeded" => geo_domain::OperationStatus::Succeeded,
        "failed" => geo_domain::OperationStatus::Failed,
        value => {
            return Err(AppError::new(
                geo_domain::ErrorCode::Internal,
                format!("invalid operation status in database: {value}"),
            ));
        }
    };
    let result = row
        .try_get::<Option<serde_json::Value>, _>("result")
        .map_err(database_unavailable)?;
    let error = row
        .try_get::<Option<serde_json::Value>, _>("error")
        .map_err(database_unavailable)?
        .map(|value| {
            serde_json::from_value(value).map_err(|_| {
                AppError::new(
                    geo_domain::ErrorCode::Internal,
                    "invalid operation error in database",
                )
            })
        })
        .transpose()?;
    Ok(Operation {
        id: row.try_get("operation_id").map_err(database_unavailable)?,
        kind: row.try_get("kind").map_err(database_unavailable)?,
        status,
        scope: TenantScope::new(
            row.try_get::<Uuid, _>("operator_id")
                .map_err(database_unavailable)?
                .into(),
            row.try_get::<Uuid, _>("tenant_id")
                .map_err(database_unavailable)?
                .into(),
            row.try_get::<Option<Uuid>, _>("project_id")
                .map_err(database_unavailable)?
                .map(Into::into),
        ),
        result,
        error,
        created_at: row.try_get("created_at").map_err(database_unavailable)?,
        updated_at: row.try_get("updated_at").map_err(database_unavailable)?,
    })
}

fn database_unavailable(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        geo_domain::ErrorCode::DependencyUnavailable,
        format!("operation persistence is unavailable: {error}"),
    )
}

fn serialization_error(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        geo_domain::ErrorCode::Internal,
        format!("operation serialization failed: {error}"),
    )
}

#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<EventEnvelope>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(256)
    }
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self { sender }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.sender.subscribe()
    }

    pub fn publish(&self, event: EventEnvelope) -> usize {
        self.sender.send(event).unwrap_or_default()
    }
}

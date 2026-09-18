use crate::{AppError, TenantScope};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
}

/// An asynchronous operation handle returned by side-effecting API commands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Operation {
    pub id: Uuid,
    pub kind: String,
    pub status: OperationStatus,
    pub scope: TenantScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AppError>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Operation {
    pub fn queued(kind: impl Into<String>, scope: TenantScope) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            kind: kind.into(),
            status: OperationStatus::Queued,
            scope,
            result: None,
            error: None,
            created_at: now,
            updated_at: now,
        }
    }
}

use crate::{OperatorId, ProjectId, TenantId, TenantScope};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Event envelope shared by durable outbox messages and the SSE feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EventEnvelope {
    pub event_id: Uuid,
    pub event_type: String,
    pub schema_version: u32,
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    /// Optional to preserve compatibility with W01/W02 event payloads that
    /// predate explicit cycle association.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle_id: Option<Uuid>,
    pub aggregate_id: Uuid,
    pub aggregate_version: u64,
    pub occurred_at: DateTime<Utc>,
    pub correlation_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_ref: Option<String>,
}

impl EventEnvelope {
    pub fn scope(&self) -> TenantScope {
        TenantScope::new(self.operator_id, self.tenant_id, self.project_id)
    }

    pub fn new(
        event_type: impl Into<String>,
        scope: TenantScope,
        aggregate_id: Uuid,
        aggregate_version: u64,
        correlation_id: Uuid,
    ) -> Self {
        Self {
            event_id: Uuid::new_v4(),
            event_type: event_type.into(),
            schema_version: 1,
            operator_id: scope.operator_id,
            tenant_id: scope.tenant_id,
            project_id: scope.project_id,
            cycle_id: None,
            aggregate_id,
            aggregate_version,
            occurred_at: Utc::now(),
            correlation_id,
            causation_id: None,
            payload_ref: None,
        }
    }
}

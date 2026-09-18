use async_trait::async_trait;
use geo_domain::{AppError, EventEnvelope, Operation};
use std::collections::HashMap;
use tokio::sync::{RwLock, broadcast};
use uuid::Uuid;

#[async_trait]
pub trait OperationStore: Send + Sync {
    async fn get(&self, id: Uuid) -> Result<Option<Operation>, AppError>;
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
}

#[async_trait]
impl OperationStore for MemoryOperationStore {
    async fn get(&self, id: Uuid) -> Result<Option<Operation>, AppError> {
        Ok(self.operations.read().await.get(&id).cloned())
    }
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

use crate::store::Store;
use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Commit first, notify second. Receivers can always recover a missed event by ID.
#[derive(Clone)]
pub struct TaskEvents {
    pub store: Arc<Store>,
    pub session_id: String,
    pub task_id: String,
    pub sender: broadcast::Sender<Value>,
}
impl TaskEvents {
    pub fn emit(&self, kind: &str, payload: Value) -> Result<Value> {
        crate::applog::task_event(kind, &self.task_id, &payload);
        let event =
            self.store
                .add_event(kind, &payload, Some(&self.session_id), Some(&self.task_id))?;
        let _ = self.sender.send(event.clone());
        Ok(event)
    }
}

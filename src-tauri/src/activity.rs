//! In-memory ring buffer of recent function invocations, surfaced on the
//! dashboard and at `GET /api/v1/activity`. Intentionally not persisted:
//! transaction records of note live on the terminal/acquirer side; this is an
//! operator convenience view.

use std::collections::VecDeque;
use std::sync::RwLock;

use serde::Serialize;
use utoipa::ToSchema;

const CAPACITY: usize = 200;

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEntry {
    pub id: String,
    /// RFC 3339 timestamp of when the invocation finished.
    pub timestamp: String,
    pub provider: String,
    pub function: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Default)]
pub struct ActivityLog {
    entries: RwLock<VecDeque<ActivityEntry>>,
}

impl ActivityLog {
    pub fn push(&self, entry: ActivityEntry) {
        let mut entries = self.entries.write().expect("activity lock");
        if entries.len() == CAPACITY {
            entries.pop_back();
        }
        entries.push_front(entry);
    }

    /// Newest first.
    pub fn list(&self, limit: usize) -> Vec<ActivityEntry> {
        let entries = self.entries.read().expect("activity lock");
        entries.iter().take(limit).cloned().collect()
    }
}

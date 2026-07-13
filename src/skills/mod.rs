use serde::{Deserialize, Serialize};

/// A normalized event produced by a protocol-v1 plugin during polling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SkillEvent {
    /// New content arrived (e.g., forum posts, messages, notifications).
    NewContent {
        /// Unique ID for deduplication
        id: String,
        /// Human-readable source (e.g., thread title, channel name)
        source: String,
        /// Who produced this content
        author: String,
        /// The content body
        body: String,
        /// Optional parent/context IDs for threading
        parent_ids: Vec<String>,
    },
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    Blocked,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "blocked" => Some(Self::Blocked),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    pub fn can_transition(from: Self, to: Self) -> bool {
        use TaskStatus::{Blocked, Cancelled, Completed, Failed, Queued, Running};

        matches!(
            (from, to),
            (Queued, Running | Cancelled)
                | (Running, Running | Blocked | Completed | Failed | Cancelled)
                | (Blocked, Running | Failed | Cancelled)
                | (Failed, Running | Failed)
        )
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRunRecord {
    pub id: String,
    pub channel: String,
    pub sender_key: String,
    pub reply_target: String,
    pub status: TaskStatus,
    pub original_request: String,
    pub last_response: Option<String>,
    pub attempt_count: u32,
    pub provider_retry_count: u32,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskEventRecord {
    pub id: i64,
    pub task_id: String,
    pub event_type: String,
    pub payload_json: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskArtifactRecord {
    pub id: i64,
    pub task_id: String,
    pub path: String,
    pub verified: bool,
    pub checksum: Option<String>,
    pub verified_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::TaskStatus;

    #[test]
    fn task_status_rejects_invalid_backward_transition() {
        assert!(TaskStatus::can_transition(
            TaskStatus::Queued,
            TaskStatus::Running
        ));
        assert!(!TaskStatus::can_transition(
            TaskStatus::Completed,
            TaskStatus::Running
        ));
    }

    #[test]
    fn task_status_parses_all_known_values() {
        assert_eq!(TaskStatus::parse("queued"), Some(TaskStatus::Queued));
        assert_eq!(TaskStatus::parse("running"), Some(TaskStatus::Running));
        assert_eq!(TaskStatus::parse("blocked"), Some(TaskStatus::Blocked));
        assert_eq!(TaskStatus::parse("completed"), Some(TaskStatus::Completed));
        assert_eq!(TaskStatus::parse("failed"), Some(TaskStatus::Failed));
        assert_eq!(TaskStatus::parse("cancelled"), Some(TaskStatus::Cancelled));
        assert_eq!(TaskStatus::parse("mystery"), None);
    }
}

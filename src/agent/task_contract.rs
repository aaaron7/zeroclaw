use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    Search,
    WriteArtifact,
    WorkspaceAnalysis,
    Mixed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    ToolSuccess,
    ArtifactVerified,
    SourceCount,
    PathAccessCheck,
    TestPass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRequirement {
    pub id: String,
    pub kind: EvidenceKind,
    pub predicate: String,
    pub min_count: u32,
    pub failure_message: String,
}

impl EvidenceRequirement {
    pub fn tool_success(tool_name: &str) -> Self {
        Self {
            id: format!("tool_success:{tool_name}"),
            kind: EvidenceKind::ToolSuccess,
            predicate: format!("tool_name={tool_name},success=true"),
            min_count: 1,
            failure_message: format!("missing successful tool evidence: {tool_name}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalMode {
    Completed,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskContract {
    pub task_type: TaskType,
    pub required_evidence: Vec<EvidenceRequirement>,
    pub acceptable_terminal_modes: Vec<TerminalMode>,
}

impl TaskContract {
    pub fn new(task_type: TaskType) -> Self {
        Self {
            task_type,
            required_evidence: Vec::new(),
            acceptable_terminal_modes: vec![TerminalMode::Completed, TerminalMode::Blocked],
        }
    }

    pub fn with_requirement(mut self, requirement: EvidenceRequirement) -> Self {
        self.required_evidence.push(requirement);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum GateDecision {
    Complete {
        reason: String,
    },
    Continue {
        missing_requirements: Vec<String>,
        reason: String,
    },
    Blocked {
        reason: String,
        remediation: String,
    },
    Failed {
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{
        EvidenceKind, EvidenceRequirement, GateDecision, TaskContract, TaskType, TerminalMode,
    };

    #[test]
    fn task_contract_holds_required_evidence_items() {
        let contract = TaskContract::new(TaskType::Search)
            .with_requirement(EvidenceRequirement::tool_success("web_search_tool"));
        assert_eq!(contract.required_evidence.len(), 1);
        assert_eq!(
            contract.required_evidence[0].kind,
            EvidenceKind::ToolSuccess
        );
    }

    #[test]
    fn gate_decision_is_explicit_and_serializable() {
        let decision = GateDecision::Continue {
            missing_requirements: vec!["search.sources>=1".to_string()],
            reason: "missing_evidence".to_string(),
        };
        let serialized = serde_json::to_string(&decision).expect("serialize gate decision");
        assert!(serialized.contains("missing_evidence"));
    }

    #[test]
    fn task_contract_defaults_to_completed_and_blocked_terminal_modes() {
        let contract = TaskContract::new(TaskType::WorkspaceAnalysis);
        assert!(contract
            .acceptable_terminal_modes
            .contains(&TerminalMode::Completed));
        assert!(contract
            .acceptable_terminal_modes
            .contains(&TerminalMode::Blocked));
    }
}

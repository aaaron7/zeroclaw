use crate::agent::evidence_ledger::EvidenceLedger;
use crate::agent::task_contract::{
    EvidenceKind, EvidenceRequirement, GateDecision, TaskContract, TaskType, TerminalMode,
};

pub struct ContractGate;

impl ContractGate {
    pub fn evaluate(
        contract: &TaskContract,
        ledger: &EvidenceLedger,
        _model_text: &str,
        _original_request: &str,
    ) -> GateDecision {
        let missing_requirements: Vec<String> = contract
            .required_evidence
            .iter()
            .filter(|requirement| !requirement_satisfied(contract, requirement, ledger))
            .map(|requirement| requirement.id.clone())
            .collect();

        if missing_requirements.is_empty() {
            return GateDecision::Complete {
                reason: "all_required_evidence_satisfied".to_string(),
            };
        }

        if contract.task_type == TaskType::WorkspaceAnalysis
            && ledger.has_access_denied_failure()
            && contract
                .acceptable_terminal_modes
                .contains(&TerminalMode::Blocked)
        {
            return GateDecision::Blocked {
                reason: "workspace_access_denied".to_string(),
                remediation: "Add the target path to `autonomy.allowed_roots` or move the project inside workspace before retrying.".to_string(),
            };
        }

        GateDecision::Continue {
            missing_requirements,
            reason: "missing_required_evidence".to_string(),
        }
    }
}

fn requirement_satisfied(
    contract: &TaskContract,
    requirement: &EvidenceRequirement,
    ledger: &EvidenceLedger,
) -> bool {
    match requirement.kind {
        EvidenceKind::ToolSuccess => {
            let Some(tool_name) = parse_tool_name(requirement) else {
                return false;
            };

            match tool_name {
                "file_write" => ledger.has_successful_write(),
                "file_read" => {
                    if contract.task_type == TaskType::WriteArtifact {
                        ledger.has_post_write_read_verification()
                    } else {
                        ledger.has_successful_read()
                    }
                }
                "web_search_tool" | "http_request" | "browser" | "browser_open" => {
                    ledger.has_successful_search()
                }
                "shell" if contract.task_type == TaskType::Search => ledger.has_successful_search(),
                name => ledger.has_successful_tool(name),
            }
        }
        _ => false,
    }
}

fn parse_tool_name(requirement: &EvidenceRequirement) -> Option<&str> {
    if let Some(tool_name) = requirement.id.strip_prefix("tool_success:") {
        let normalized = tool_name.trim();
        if !normalized.is_empty() {
            return Some(normalized);
        }
    }

    requirement
        .predicate
        .split(',')
        .find_map(|segment| segment.trim().strip_prefix("tool_name="))
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::ContractGate;
    use crate::agent::evidence_ledger::collect_evidence_from_history;
    use crate::agent::task_contract::{EvidenceRequirement, GateDecision, TaskContract, TaskType};
    use crate::providers::ChatMessage;

    #[test]
    fn contract_gate_search_without_search_evidence_continues() {
        let contract = TaskContract::new(TaskType::Search)
            .with_requirement(EvidenceRequirement::tool_success("web_search_tool"));
        let ledger = collect_evidence_from_history(&[ChatMessage::user("直接开搜")]);

        let decision = ContractGate::evaluate(&contract, &ledger, "我来搜索", "直接开搜");
        match decision {
            GateDecision::Continue {
                missing_requirements,
                reason,
            } => {
                assert_eq!(reason, "missing_required_evidence");
                assert!(missing_requirements.contains(&"tool_success:web_search_tool".to_string()));
            }
            other => panic!("expected continue, got {other:?}"),
        }
    }

    #[test]
    fn contract_gate_write_with_verified_artifact_completes() {
        let contract = TaskContract::new(TaskType::WriteArtifact)
            .with_requirement(EvidenceRequirement::tool_success("file_write"))
            .with_requirement(EvidenceRequirement::tool_success("file_read"));
        let history = vec![
            ChatMessage::assistant(
                r#"<tool_call>
{"name":"file_write","arguments":{"path":"report.md","content":"abc"}}
</tool_call>"#,
            ),
            ChatMessage::user(
                "[Tool results]\n<tool_result name=\"file_write\">\nWritten 3 bytes\n</tool_result>",
            ),
            ChatMessage::assistant(
                r#"<tool_call>
{"name":"file_read","arguments":{"path":"report.md"}}
</tool_call>"#,
            ),
            ChatMessage::user("[Tool results]\n<tool_result name=\"file_read\">\nabc\n</tool_result>"),
        ];
        let ledger = collect_evidence_from_history(&history);

        let decision = ContractGate::evaluate(&contract, &ledger, "已完成", "保存报告");
        match decision {
            GateDecision::Complete { reason } => {
                assert_eq!(reason, "all_required_evidence_satisfied");
            }
            other => panic!("expected complete, got {other:?}"),
        }
    }

    #[test]
    fn contract_gate_workspace_access_denied_returns_blocked() {
        let contract = TaskContract::new(TaskType::WorkspaceAnalysis)
            .with_requirement(EvidenceRequirement::tool_success("file_read"));
        let history = vec![
            ChatMessage::assistant(
                r#"<tool_call>
{"name":"file_read","arguments":{"path":"studio/"}}
</tool_call>"#,
            ),
            ChatMessage::user(
                "[Tool results]\n<tool_result name=\"file_read\">\nERROR: path not allowed outside workspace\n</tool_result>",
            ),
        ];
        let ledger = collect_evidence_from_history(&history);

        let decision = ContractGate::evaluate(&contract, &ledger, "我分析好了", "分析 studio");
        match decision {
            GateDecision::Blocked {
                reason,
                remediation,
            } => {
                assert_eq!(reason, "workspace_access_denied");
                assert!(remediation.contains("allowed_roots"));
            }
            other => panic!("expected blocked, got {other:?}"),
        }
    }

    #[test]
    fn contract_gate_workspace_without_denied_evidence_keeps_continuing() {
        let contract = TaskContract::new(TaskType::WorkspaceAnalysis)
            .with_requirement(EvidenceRequirement::tool_success("file_read"));
        let ledger = collect_evidence_from_history(&[ChatMessage::user("分析 studio")]);

        let decision = ContractGate::evaluate(&contract, &ledger, "我继续分析", "分析 studio");
        match decision {
            GateDecision::Continue {
                missing_requirements,
                ..
            } => {
                assert!(missing_requirements.contains(&"tool_success:file_read".to_string()));
            }
            other => panic!("expected continue, got {other:?}"),
        }
    }
}

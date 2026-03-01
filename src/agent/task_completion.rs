use crate::agent::contract_gate::ContractGate;
use crate::agent::evidence_ledger::collect_evidence_from_history;
use crate::agent::task_contract::{GateDecision, TaskContract, TaskType};
use crate::providers::ChatMessage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionDecision {
    Complete,
    Continue {
        reason: String,
        missing_requirements: Vec<String>,
    },
    Blocked {
        reason: String,
        remediation: String,
    },
    Failed {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionEvaluation {
    pub decision: CompletionDecision,
    pub saw_successful_write: bool,
    pub saw_post_write_read_after_success: bool,
}

pub fn evaluate_completion(
    contract: &TaskContract,
    response_text: &str,
    history: &[ChatMessage],
    original_request: &str,
) -> CompletionEvaluation {
    let evidence = collect_evidence_from_history(history);

    let decision = if response_text.contains("[Guardrail Notice]") {
        CompletionDecision::Continue {
            reason: "guardrail_notice".to_string(),
            missing_requirements: Vec::new(),
        }
    } else {
        let gate_decision =
            ContractGate::evaluate(contract, &evidence, response_text, original_request);
        if matches!(gate_decision, GateDecision::Complete { .. })
            && contract.task_type == TaskType::Unknown
            && !has_any_tool_evidence(&evidence)
            && looks_like_non_terminal_update(response_text)
        {
            CompletionDecision::Continue {
                reason: "unknown_contract_non_terminal_update".to_string(),
                missing_requirements: Vec::new(),
            }
        } else {
            match gate_decision {
                GateDecision::Complete { .. } => CompletionDecision::Complete,
                GateDecision::Continue {
                    reason,
                    missing_requirements,
                } => CompletionDecision::Continue {
                    reason,
                    missing_requirements,
                },
                GateDecision::Blocked {
                    reason,
                    remediation,
                } => CompletionDecision::Blocked {
                    reason,
                    remediation,
                },
                GateDecision::Failed { reason } => CompletionDecision::Failed { reason },
            }
        }
    };

    CompletionEvaluation {
        decision,
        saw_successful_write: evidence.has_successful_write(),
        saw_post_write_read_after_success: evidence.has_post_write_read_verification(),
    }
}

fn has_any_tool_evidence(evidence: &crate::agent::evidence_ledger::EvidenceLedger) -> bool {
    evidence.has_successful_write()
        || evidence.has_successful_read()
        || evidence.has_successful_search()
}

fn looks_like_non_terminal_update(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let progress_hints = [
        "i'm checking",
        "i am checking",
        "let me check",
        "let me search",
        "i will search",
        "i'll search",
        "working on",
        "我正在",
        "我先",
        "让我先",
        "我会",
        "我这就",
        "马上给你",
        "继续处理中",
    ];
    let completion_hints = [
        "done",
        "completed",
        "finished",
        "任务完成",
        "已完成",
        "已经完成",
        "完成了",
        "已写入",
        "已经写入",
        "已保存",
        "已经保存",
        "成功创建",
        "已生成",
        "已经生成",
    ];

    progress_hints
        .iter()
        .any(|hint| lower.contains(hint) || text.contains(hint))
        && !completion_hints
            .iter()
            .any(|hint| lower.contains(hint) || text.contains(hint))
}

#[cfg(test)]
mod tests {
    use super::{evaluate_completion, CompletionDecision};
    use crate::agent::task_contract::{EvidenceRequirement, TaskContract, TaskType};
    use crate::providers::ChatMessage;

    #[test]
    fn completion_evaluator_requires_write_evidence_even_when_model_claims_saved() {
        let contract = TaskContract::new(TaskType::WriteArtifact)
            .with_requirement(EvidenceRequirement::tool_success("file_write"))
            .with_requirement(EvidenceRequirement::tool_success("file_read"));
        let history = vec![ChatMessage::user("帮我保存报告到工作空间")];

        let eval = evaluate_completion(
            &contract,
            "好的，我已经保存到 report.md。",
            &history,
            "保存报告",
        );
        assert_eq!(
            eval.decision,
            CompletionDecision::Continue {
                reason: "missing_required_evidence".to_string(),
                missing_requirements: vec![
                    "tool_success:file_write".to_string(),
                    "tool_success:file_read".to_string(),
                ],
            }
        );
    }

    #[test]
    fn completion_evaluator_accepts_write_after_post_write_verification() {
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
                "[Tool results]\n<tool_result name=\"file_write\">\nWritten 3 bytes to report.md\n</tool_result>",
            ),
            ChatMessage::assistant(
                r#"<tool_call>
{"name":"file_read","arguments":{"path":"report.md"}}
</tool_call>"#,
            ),
            ChatMessage::user("[Tool results]\n<tool_result name=\"file_read\">\nabc\n</tool_result>"),
        ];

        let eval = evaluate_completion(&contract, "报告已保存到 report.md。", &history, "保存报告");
        assert_eq!(eval.decision, CompletionDecision::Complete);
        assert!(eval.saw_successful_write);
        assert!(eval.saw_post_write_read_after_success);
    }

    #[test]
    fn completion_evaluator_marks_workspace_denied_as_blocked() {
        let contract = TaskContract::new(TaskType::WorkspaceAnalysis)
            .with_requirement(EvidenceRequirement::tool_success("file_read"));
        let history = vec![
            ChatMessage::assistant(
                r#"<tool_call>
{"name":"file_read","arguments":{"path":"studio"}}
</tool_call>"#,
            ),
            ChatMessage::user(
                "[Tool results]\n<tool_result name=\"file_read\">\nERROR: path not allowed outside workspace\n</tool_result>",
            ),
        ];

        let eval = evaluate_completion(&contract, "我继续分析", &history, "分析 studio");
        assert_eq!(
            eval.decision,
            CompletionDecision::Blocked {
                reason: "workspace_access_denied".to_string(),
                remediation: "Add the target path to `autonomy.allowed_roots` or move the project inside workspace before retrying.".to_string(),
            }
        );
    }

    #[test]
    fn completion_evaluator_keeps_unknown_contract_running_for_progress_only_update() {
        let contract = TaskContract::new(TaskType::Unknown);
        let history = vec![ChatMessage::user("继续处理")];

        let eval = evaluate_completion(&contract, "我正在检查当前文件状态。", &history, "继续处理");
        assert_eq!(
            eval.decision,
            CompletionDecision::Continue {
                reason: "unknown_contract_non_terminal_update".to_string(),
                missing_requirements: Vec::new(),
            }
        );
    }
}

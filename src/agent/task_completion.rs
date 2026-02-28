use crate::agent::evidence_ledger::collect_evidence_from_history;
use crate::providers::ChatMessage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionDecision {
    Complete,
    Continue { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionEvaluation {
    pub decision: CompletionDecision,
    pub saw_successful_write: bool,
    pub saw_post_write_read_after_success: bool,
}

pub fn evaluate_completion(response_text: &str, history: &[ChatMessage]) -> CompletionEvaluation {
    let evidence = collect_evidence_from_history(history);

    if response_text.contains("[Guardrail Notice]") {
        return CompletionEvaluation {
            decision: CompletionDecision::Continue {
                reason: "guardrail_notice".to_string(),
            },
            saw_successful_write: evidence.has_successful_write(),
            saw_post_write_read_after_success: evidence.has_post_write_read_verification(),
        };
    }

    if looks_like_filesystem_write_claim(response_text)
        && !evidence.has_post_write_read_verification()
    {
        return CompletionEvaluation {
            decision: CompletionDecision::Continue {
                reason: "write_claim_without_post_write_verification".to_string(),
            },
            saw_successful_write: evidence.has_successful_write(),
            saw_post_write_read_after_success: evidence.has_post_write_read_verification(),
        };
    }

    if looks_like_in_progress_update(response_text) {
        return CompletionEvaluation {
            decision: CompletionDecision::Continue {
                reason: "in_progress_update".to_string(),
            },
            saw_successful_write: evidence.has_successful_write(),
            saw_post_write_read_after_success: evidence.has_post_write_read_verification(),
        };
    }

    CompletionEvaluation {
        decision: CompletionDecision::Complete,
        saw_successful_write: evidence.has_successful_write(),
        saw_post_write_read_after_success: evidence.has_post_write_read_verification(),
    }
}

fn looks_like_filesystem_write_claim(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let chinese_claim_hints = [
        "已写入",
        "已经写入",
        "写到了",
        "已保存",
        "已经保存",
        "保存到",
        "保存在",
        "保存于",
        "已存储",
        "已经存储",
        "存储到",
        "已创建",
        "已经创建",
        "成功创建",
        "已成功创建",
        "文件已成功创建",
        "已生成",
        "已经生成",
        "已更新",
        "已经更新",
    ];
    if chinese_claim_hints.iter().any(|hint| text.contains(hint)) {
        return true;
    }

    let completion_verbs = [
        "i wrote",
        "written to",
        "saved to",
        "saved as",
        "has been saved",
        "has been written",
        "created at",
        "created the file",
        "updated the file",
        "generated the report",
        "i updated",
        "i created",
        "i saved",
    ];

    let file_indicators = [
        "/", "\\", ".md", ".txt", ".rs", ".json", ".yaml", ".yml", ".toml", " file ", " path ",
        "docs/", "src/",
    ];

    completion_verbs.iter().any(|hint| lower.contains(hint))
        && file_indicators.iter().any(|hint| lower.contains(hint))
}

fn looks_like_in_progress_update(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();

    let completion_hints = [
        "done",
        "completed",
        "finished",
        "successfully",
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
        "已更新",
        "已经更新",
    ];
    if completion_hints
        .iter()
        .any(|hint| lower.contains(hint) || text.contains(hint))
    {
        return false;
    }

    let progress_hints = [
        "i'm checking",
        "let me check",
        "i am checking",
        "i'm reviewing",
        "let me review",
        "i need to inspect",
        "working on",
        "currently implementing",
        "我正在",
        "让我检查",
        "我先检查",
        "让我先查看",
        "我需要先查看",
        "正在实施",
    ];

    progress_hints
        .iter()
        .any(|hint| lower.contains(hint) || text.contains(hint))
}

#[cfg(test)]
mod tests {
    use super::{evaluate_completion, CompletionDecision};
    use crate::providers::ChatMessage;

    #[test]
    fn completion_evaluator_blocks_write_claim_without_evidence() {
        let history = vec![
            ChatMessage::assistant(
                r#"<tool_call>
{"name":"file_write","arguments":{"path":"a.md","content":"x"}}
</tool_call>"#,
            ),
            ChatMessage::user("[Tool results]\n<tool_result name=\"file_write\">\nAction blocked: denied\n</tool_result>"),
        ];

        let eval = evaluate_completion("好的，我已经保存到 a.md。", &history);
        assert_eq!(
            eval.decision,
            CompletionDecision::Continue {
                reason: "write_claim_without_post_write_verification".to_string()
            }
        );
        assert!(!eval.saw_successful_write);
        assert!(!eval.saw_post_write_read_after_success);
    }

    #[test]
    fn completion_evaluator_accepts_write_claim_after_post_write_read_verification() {
        let history = vec![
            ChatMessage::assistant(
                r#"<tool_call>
{"name":"file_write","arguments":{"path":"report.md","content":"abc"}}
</tool_call>"#,
            ),
            ChatMessage::user("[Tool results]\n<tool_result name=\"file_write\">\nWritten 3 bytes to report.md\n</tool_result>"),
            ChatMessage::assistant(
                r#"<tool_call>
{"name":"file_read","arguments":{"path":"report.md"}}
</tool_call>"#,
            ),
            ChatMessage::user(
                "[Tool results]\n<tool_result name=\"file_read\">\nabc\n</tool_result>",
            ),
        ];

        let eval = evaluate_completion("报告已保存到 report.md。", &history);
        assert_eq!(eval.decision, CompletionDecision::Complete);
        assert!(eval.saw_successful_write);
        assert!(eval.saw_post_write_read_after_success);
    }

    #[test]
    fn completion_evaluator_detects_in_progress_update() {
        let history = vec![ChatMessage::user("帮我继续实现")];
        let eval = evaluate_completion("我正在检查当前文件状态。", &history);
        assert_eq!(
            eval.decision,
            CompletionDecision::Continue {
                reason: "in_progress_update".to_string()
            }
        );
    }
}

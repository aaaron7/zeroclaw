use crate::providers::ChatMessage;
use std::collections::VecDeque;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellToolKind {
    WriteLike,
    ReadLike,
    Other,
}

pub fn evaluate_completion(response_text: &str, history: &[ChatMessage]) -> CompletionEvaluation {
    let evidence = collect_tool_evidence(history);

    if response_text.contains("[Guardrail Notice]") {
        return CompletionEvaluation {
            decision: CompletionDecision::Continue {
                reason: "guardrail_notice".to_string(),
            },
            saw_successful_write: evidence.saw_successful_write,
            saw_post_write_read_after_success: evidence.saw_post_write_read_after_success,
        };
    }

    if looks_like_filesystem_write_claim(response_text)
        && !evidence.saw_post_write_read_after_success
    {
        return CompletionEvaluation {
            decision: CompletionDecision::Continue {
                reason: "write_claim_without_post_write_verification".to_string(),
            },
            saw_successful_write: evidence.saw_successful_write,
            saw_post_write_read_after_success: evidence.saw_post_write_read_after_success,
        };
    }

    if looks_like_in_progress_update(response_text) {
        return CompletionEvaluation {
            decision: CompletionDecision::Continue {
                reason: "in_progress_update".to_string(),
            },
            saw_successful_write: evidence.saw_successful_write,
            saw_post_write_read_after_success: evidence.saw_post_write_read_after_success,
        };
    }

    CompletionEvaluation {
        decision: CompletionDecision::Complete,
        saw_successful_write: evidence.saw_successful_write,
        saw_post_write_read_after_success: evidence.saw_post_write_read_after_success,
    }
}

#[derive(Default)]
struct ToolEvidence {
    saw_successful_write: bool,
    saw_post_write_read_after_success: bool,
}

fn collect_tool_evidence(history: &[ChatMessage]) -> ToolEvidence {
    let mut evidence = ToolEvidence::default();
    let mut shell_kinds = VecDeque::new();

    for msg in history {
        match msg.role.as_str() {
            "assistant" => {
                collect_shell_tool_kinds_from_assistant_calls(&msg.content, &mut shell_kinds);
            }
            "user" => {
                collect_tool_result_evidence(
                    &msg.content,
                    &mut shell_kinds,
                    &mut evidence.saw_successful_write,
                    &mut evidence.saw_post_write_read_after_success,
                );
            }
            _ => {}
        }
    }

    evidence
}

fn collect_shell_tool_kinds_from_assistant_calls(content: &str, out: &mut VecDeque<ShellToolKind>) {
    const TAG_PAIRS: [(&str, &str); 4] = [
        ("<tool_call>", "</tool_call>"),
        ("<toolcall>", "</toolcall>"),
        ("<tool-call>", "</tool-call>"),
        ("<invoke>", "</invoke>"),
    ];

    for (open_tag, close_tag) in TAG_PAIRS {
        for segment in content.split(open_tag) {
            let Some(json_end) = segment.find(close_tag) else {
                continue;
            };
            let json_str = segment[..json_end].trim();
            let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) else {
                continue;
            };
            let Some(name) = val.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            if name != "shell" {
                continue;
            }
            let shell_kind = val
                .get("arguments")
                .and_then(|args| args.get("command"))
                .and_then(|cmd| cmd.as_str())
                .map(classify_shell_command)
                .unwrap_or(ShellToolKind::Other);
            out.push_back(shell_kind);
        }
    }
}

fn collect_tool_result_evidence(
    content: &str,
    shell_kinds: &mut VecDeque<ShellToolKind>,
    saw_successful_write: &mut bool,
    saw_post_write_read_after_success: &mut bool,
) {
    let marker = "<tool_result name=\"";
    let mut remaining = content;

    while let Some(start) = remaining.find(marker) {
        let name_start = start + marker.len();
        let after_name_start = &remaining[name_start..];
        let Some(name_end) = after_name_start.find('"') else {
            break;
        };
        let tool_name = &after_name_start[..name_end];
        let after_tag_start = &after_name_start[name_end..];
        let Some(body_start) = after_tag_start.find('>') else {
            break;
        };
        let after_body_start = &after_tag_start[body_start + 1..];
        let Some(close_idx) = after_body_start.find("</tool_result>") else {
            break;
        };
        let output = after_body_start[..close_idx].trim();
        let is_success = !tool_result_output_likely_failure(output);

        let kind = match tool_name {
            "file_write" => ShellToolKind::WriteLike,
            "file_read" => ShellToolKind::ReadLike,
            "shell" => shell_kinds.pop_front().unwrap_or(ShellToolKind::Other),
            _ => ShellToolKind::Other,
        };

        if kind == ShellToolKind::WriteLike && is_success {
            *saw_successful_write = true;
        }
        if kind == ShellToolKind::ReadLike && is_success && *saw_successful_write {
            *saw_post_write_read_after_success = true;
        }

        remaining = &after_body_start[close_idx + "</tool_result>".len()..];
    }
}

fn tool_result_output_likely_failure(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    lower.contains("failed")
        || lower.contains("error")
        || lower.contains("not allowed")
        || lower.contains("denied")
        || lower.contains("missing")
        || lower.contains("refusing")
}

fn classify_shell_command(command: &str) -> ShellToolKind {
    let lower = command.to_ascii_lowercase();

    if lower.contains(">>")
        || lower.contains(" > ")
        || lower.contains("\n>")
        || lower.contains("tee ")
        || lower.contains("touch ")
        || lower.contains("mkdir ")
        || lower.contains("cp ")
        || lower.contains("mv ")
        || lower.contains("truncate ")
        || lower.contains("sed -i")
        || lower.contains("perl -i")
    {
        return ShellToolKind::WriteLike;
    }

    if lower.contains("cat ")
        || lower.contains("less ")
        || lower.contains("more ")
        || lower.contains("head ")
        || lower.contains("tail ")
        || lower.contains("wc ")
        || lower.contains("stat ")
        || lower.contains("ls ")
        || lower.contains("find ")
        || lower.contains("rg ")
        || lower.contains("grep ")
        || lower.contains("sed -n")
        || lower.contains("nl ")
    {
        return ShellToolKind::ReadLike;
    }

    ShellToolKind::Other
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

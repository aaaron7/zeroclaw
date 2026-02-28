use crate::providers::ChatMessage;
use std::collections::VecDeque;

#[derive(Debug, Clone, Default)]
pub struct EvidenceLedger {
    saw_successful_write: bool,
    saw_successful_read: bool,
    saw_successful_search: bool,
    saw_post_write_read_verification: bool,
}

impl EvidenceLedger {
    pub fn has_successful_write(&self) -> bool {
        self.saw_successful_write
    }

    pub fn has_successful_read(&self) -> bool {
        self.saw_successful_read
    }

    pub fn has_successful_search(&self) -> bool {
        self.saw_successful_search
    }

    pub fn has_post_write_read_verification(&self) -> bool {
        self.saw_post_write_read_verification
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolKind {
    WriteLike,
    ReadLike,
    SearchLike,
    Other,
}

pub fn collect_evidence_from_history(history: &[ChatMessage]) -> EvidenceLedger {
    let mut ledger = EvidenceLedger::default();
    let mut shell_kinds = VecDeque::new();

    for msg in history {
        match msg.role.as_str() {
            "assistant" => {
                collect_shell_tool_kinds_from_assistant_calls(&msg.content, &mut shell_kinds);
            }
            "user" => {
                collect_tool_result_evidence(&msg.content, &mut shell_kinds, &mut ledger);
            }
            _ => {}
        }
    }

    ledger
}

fn collect_shell_tool_kinds_from_assistant_calls(content: &str, out: &mut VecDeque<ToolKind>) {
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
                .unwrap_or(ToolKind::Other);
            out.push_back(shell_kind);
        }
    }
}

fn collect_tool_result_evidence(
    content: &str,
    shell_kinds: &mut VecDeque<ToolKind>,
    ledger: &mut EvidenceLedger,
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
            "file_write" => ToolKind::WriteLike,
            "file_read" | "glob_search" | "content_search" | "pdf_read" => ToolKind::ReadLike,
            "web_search_tool" | "http_request" | "browser" | "browser_open" => ToolKind::SearchLike,
            "shell" => shell_kinds.pop_front().unwrap_or(ToolKind::Other),
            _ => ToolKind::Other,
        };

        if kind == ToolKind::WriteLike && is_success {
            ledger.saw_successful_write = true;
        }
        if kind == ToolKind::ReadLike && is_success {
            ledger.saw_successful_read = true;
        }
        if kind == ToolKind::ReadLike && is_success && ledger.saw_successful_write {
            ledger.saw_post_write_read_verification = true;
        }
        if kind == ToolKind::SearchLike && is_success {
            ledger.saw_successful_search = true;
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

fn classify_shell_command(command: &str) -> ToolKind {
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
        return ToolKind::WriteLike;
    }

    if lower.contains("curl ")
        || lower.contains("wget ")
        || lower.contains("http://")
        || lower.contains("https://")
    {
        return ToolKind::SearchLike;
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
        return ToolKind::ReadLike;
    }

    ToolKind::Other
}

#[cfg(test)]
mod tests {
    use super::collect_evidence_from_history;
    use crate::providers::ChatMessage;

    #[test]
    fn evidence_ledger_collects_web_search_tool_success() {
        let history = vec![
            ChatMessage::assistant(
                r#"<tool_call>
{"name":"web_search_tool","arguments":{"query":"today news"}}
</tool_call>"#,
            ),
            ChatMessage::user(
                "[Tool results]\n<tool_result name=\"web_search_tool\">\nTop headlines...\n</tool_result>",
            ),
        ];

        let ledger = collect_evidence_from_history(&history);
        assert!(
            ledger.has_successful_search(),
            "search evidence should be captured"
        );
    }

    #[test]
    fn evidence_ledger_collects_write_and_post_write_read_verification() {
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
        assert!(ledger.has_successful_write());
        assert!(ledger.has_post_write_read_verification());
    }

    #[test]
    fn evidence_ledger_collects_shell_curl_as_search_evidence() {
        let history = vec![
            ChatMessage::assistant(
                r#"<tool_call>
{"name":"shell","arguments":{"command":"curl https://example.com"}}
</tool_call>"#,
            ),
            ChatMessage::user(
                "[Tool results]\n<tool_result name=\"shell\">\n<html>ok</html>\n</tool_result>",
            ),
        ];

        let ledger = collect_evidence_from_history(&history);
        assert!(ledger.has_successful_search());
    }

    #[test]
    fn evidence_ledger_does_not_count_failed_tool_result_as_success() {
        let history = vec![
            ChatMessage::assistant(
                r#"<tool_call>
{"name":"web_search_tool","arguments":{"query":"today news"}}
</tool_call>"#,
            ),
            ChatMessage::user(
                "[Tool results]\n<tool_result name=\"web_search_tool\">\nERROR: denied\n</tool_result>",
            ),
        ];

        let ledger = collect_evidence_from_history(&history);
        assert!(!ledger.has_successful_search());
    }
}

use crate::providers::ChatMessage;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Default)]
pub struct EvidenceLedger {
    saw_successful_write: bool,
    saw_successful_read: bool,
    saw_successful_search: bool,
    saw_post_write_read_verification: bool,
    successful_tools: HashSet<String>,
    failed_tools: HashSet<String>,
    saw_access_denied_failure: bool,
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

    pub fn has_successful_tool(&self, tool_name: &str) -> bool {
        self.successful_tools
            .contains(&tool_name.trim().to_ascii_lowercase())
    }

    pub fn has_failed_tool(&self, tool_name: &str) -> bool {
        self.failed_tools
            .contains(&tool_name.trim().to_ascii_lowercase())
    }

    pub fn has_access_denied_failure(&self) -> bool {
        self.saw_access_denied_failure
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolKind {
    WriteLike,
    ReadLike,
    SearchLike,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedToolCall {
    name: String,
    kind: ToolKind,
}

pub fn collect_evidence_from_history(history: &[ChatMessage]) -> EvidenceLedger {
    let mut ledger = EvidenceLedger::default();
    let mut queued_calls = VecDeque::new();
    let mut calls_by_id = HashMap::new();

    for msg in history {
        match msg.role.as_str() {
            "assistant" => {
                collect_assistant_tool_calls(&msg.content, &mut queued_calls, &mut calls_by_id);
            }
            "user" => {
                collect_prompt_tool_result_evidence(&msg.content, &mut queued_calls, &mut ledger);
            }
            "tool" => {
                collect_native_tool_result_evidence(
                    &msg.content,
                    &mut queued_calls,
                    &calls_by_id,
                    &mut ledger,
                );
            }
            _ => {}
        }
    }

    ledger
}

fn collect_assistant_tool_calls(
    content: &str,
    queued_calls: &mut VecDeque<ObservedToolCall>,
    calls_by_id: &mut HashMap<String, ObservedToolCall>,
) {
    collect_assistant_tool_calls_from_xml(content, queued_calls, calls_by_id);
    collect_assistant_tool_calls_from_native_json(content, queued_calls, calls_by_id);
}

fn collect_assistant_tool_calls_from_xml(
    content: &str,
    queued_calls: &mut VecDeque<ObservedToolCall>,
    calls_by_id: &mut HashMap<String, ObservedToolCall>,
) {
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
            let Some(name) = val.get("name").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let call = observed_tool_call_from_name_and_args(name, val.get("arguments"));
            if let Some(call_id) = try_parse_call_id(&val) {
                calls_by_id.insert(call_id, call.clone());
            }
            queued_calls.push_back(call);
        }
    }
}

fn collect_assistant_tool_calls_from_native_json(
    content: &str,
    queued_calls: &mut VecDeque<ObservedToolCall>,
    calls_by_id: &mut HashMap<String, ObservedToolCall>,
) {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(content) else {
        return;
    };
    let Some(tool_calls) = val.get("tool_calls").and_then(serde_json::Value::as_array) else {
        return;
    };

    for call in tool_calls {
        let Some(name) = call.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let observed = observed_tool_call_from_name_and_args(name, call.get("arguments"));
        if let Some(call_id) = try_parse_call_id(call) {
            calls_by_id.insert(call_id, observed.clone());
        }
        queued_calls.push_back(observed);
    }
}

fn extract_shell_command_from_arguments(arguments: Option<&serde_json::Value>) -> Option<String> {
    let args = arguments?;
    match args {
        serde_json::Value::Object(_) => args
            .get("command")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        serde_json::Value::String(raw) => serde_json::from_str::<serde_json::Value>(raw)
            .ok()
            .and_then(|parsed| {
                parsed
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string)
            }),
        _ => None,
    }
}

fn try_parse_call_id(value: &serde_json::Value) -> Option<String> {
    value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            value
                .get("tool_call_id")
                .and_then(serde_json::Value::as_str)
        })
        .map(ToString::to_string)
}

fn observed_tool_call_from_name_and_args(
    tool_name: &str,
    arguments: Option<&serde_json::Value>,
) -> ObservedToolCall {
    if tool_name == "shell" {
        let shell_kind = extract_shell_command_from_arguments(arguments)
            .as_deref()
            .map(classify_shell_command)
            .unwrap_or(ToolKind::Other);
        return ObservedToolCall {
            name: "shell".to_string(),
            kind: shell_kind,
        };
    }
    ObservedToolCall {
        name: tool_name.to_string(),
        kind: classify_tool_kind(tool_name),
    }
}

fn classify_tool_kind(tool_name: &str) -> ToolKind {
    match tool_name {
        "file_write" => ToolKind::WriteLike,
        "file_read" | "glob_search" | "content_search" | "pdf_read" => ToolKind::ReadLike,
        "web_search_tool" | "http_request" | "browser" | "browser_open" => ToolKind::SearchLike,
        _ => ToolKind::Other,
    }
}

fn collect_prompt_tool_result_evidence(
    content: &str,
    queued_calls: &mut VecDeque<ObservedToolCall>,
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
        let normalized = tool_name.trim().to_ascii_lowercase();
        let call = if normalized == "shell" {
            queued_calls
                .pop_front()
                .unwrap_or_else(|| ObservedToolCall {
                    name: "shell".to_string(),
                    kind: ToolKind::Other,
                })
        } else if queued_calls
            .front()
            .is_some_and(|next| next.name.eq_ignore_ascii_case(&normalized))
        {
            queued_calls
                .pop_front()
                .unwrap_or_else(|| ObservedToolCall {
                    name: normalized.clone(),
                    kind: classify_tool_kind(&normalized),
                })
        } else {
            ObservedToolCall {
                name: normalized,
                kind: classify_tool_kind(tool_name),
            }
        };
        apply_tool_result_event(call, output, ledger);

        remaining = &after_body_start[close_idx + "</tool_result>".len()..];
    }
}

fn collect_native_tool_result_evidence(
    content: &str,
    queued_calls: &mut VecDeque<ObservedToolCall>,
    calls_by_id: &HashMap<String, ObservedToolCall>,
    ledger: &mut EvidenceLedger,
) {
    let (tool_call_id, output) =
        parse_tool_message_payload(content).unwrap_or_else(|| (None, content.trim().to_string()));

    let call = tool_call_id
        .as_deref()
        .and_then(|id| calls_by_id.get(id).cloned())
        .or_else(|| queued_calls.pop_front())
        .unwrap_or_else(|| ObservedToolCall {
            name: "unknown".to_string(),
            kind: ToolKind::Other,
        });
    apply_tool_result_event(call, &output, ledger);
}

fn parse_tool_message_payload(content: &str) -> Option<(Option<String>, String)> {
    let val = serde_json::from_str::<serde_json::Value>(content).ok()?;
    let obj = val.as_object()?;
    let tool_call_id = obj
        .get("tool_call_id")
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string);
    let output = obj
        .get("content")
        .map(json_value_to_text)
        .unwrap_or_else(|| content.trim().to_string());
    Some((tool_call_id, output))
}

fn json_value_to_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| String::new()),
    }
}

fn apply_tool_result_event(call: ObservedToolCall, output: &str, ledger: &mut EvidenceLedger) {
    let is_success = !tool_result_output_likely_failure(output);
    let normalized_name = call.name.trim().to_ascii_lowercase();

    if is_success {
        ledger.successful_tools.insert(normalized_name);
    } else {
        ledger.failed_tools.insert(normalized_name);
        if tool_result_output_likely_access_denied(output) {
            ledger.saw_access_denied_failure = true;
        }
    }

    if call.kind == ToolKind::WriteLike && is_success {
        ledger.saw_successful_write = true;
    }
    if call.kind == ToolKind::ReadLike && is_success {
        ledger.saw_successful_read = true;
    }
    if call.kind == ToolKind::ReadLike && is_success && ledger.saw_successful_write {
        ledger.saw_post_write_read_verification = true;
    }
    if call.kind == ToolKind::SearchLike && is_success {
        ledger.saw_successful_search = true;
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

fn tool_result_output_likely_access_denied(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    lower.contains("permission denied")
        || lower.contains("path not allowed")
        || lower.contains("not allowed")
        || lower.contains("access denied")
        || lower.contains("outside workspace")
        || lower.contains("forbidden")
        || output.contains("没有权限")
        || output.contains("权限不足")
        || output.contains("路径不允许")
        || output.contains("无法访问")
        || output.contains("拒绝访问")
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

    #[test]
    fn evidence_ledger_marks_workspace_access_denied_failures() {
        let history = vec![
            ChatMessage::assistant(
                r#"<tool_call>
{"name":"file_read","arguments":{"path":"/private/repo"}}
</tool_call>"#,
            ),
            ChatMessage::user(
                "[Tool results]\n<tool_result name=\"file_read\">\nERROR: path not allowed outside workspace\n</tool_result>",
            ),
        ];

        let ledger = collect_evidence_from_history(&history);
        assert!(ledger.has_failed_tool("file_read"));
        assert!(ledger.has_access_denied_failure());
    }

    #[test]
    fn evidence_ledger_collects_search_from_native_tool_messages() {
        let history = vec![
            ChatMessage::assistant(
                r#"{"content":"","tool_calls":[{"id":"call_web_1","name":"web_search_tool","arguments":{"query":"ai agent skills"}}]}"#,
            ),
            ChatMessage::tool(
                r#"{"tool_call_id":"call_web_1","content":"Top repositories:\n1. example/repo"}"#,
            ),
        ];

        let ledger = collect_evidence_from_history(&history);
        assert!(
            ledger.has_successful_search(),
            "native role=tool results should count as search evidence"
        );
        assert!(
            ledger.has_successful_tool("web_search_tool"),
            "native role=tool results should preserve successful tool name"
        );
    }
}

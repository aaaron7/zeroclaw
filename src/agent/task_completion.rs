use crate::providers::ChatMessage;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionDecision {
    Complete,
    Continue { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionEvaluation {
    pub decision: CompletionDecision,
    pub saw_successful_write: bool,
    pub saw_successful_read: bool,
    pub saw_successful_search: bool,
    pub saw_successful_browser: bool,
    pub saw_post_write_read_after_success: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellToolKind {
    WriteLike,
    ReadLike,
    SearchLike,
    Other,
}

pub fn evaluate_completion(response_text: &str, history: &[ChatMessage]) -> CompletionEvaluation {
    evaluate_completion_with_request(response_text, history, None)
}

pub fn evaluate_completion_with_request(
    response_text: &str,
    history: &[ChatMessage],
    original_request: Option<&str>,
) -> CompletionEvaluation {
    let evidence = collect_tool_evidence_snapshot(history);
    let intent = infer_task_intent(original_request);

    if response_text.contains("[Guardrail Notice]") {
        return CompletionEvaluation {
            decision: CompletionDecision::Continue {
                reason: "guardrail_notice".to_string(),
            },
            saw_successful_write: evidence.saw_successful_write,
            saw_successful_read: evidence.saw_successful_read,
            saw_successful_search: evidence.saw_successful_search,
            saw_successful_browser: evidence.saw_successful_browser,
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
            saw_successful_read: evidence.saw_successful_read,
            saw_successful_search: evidence.saw_successful_search,
            saw_successful_browser: evidence.saw_successful_browser,
            saw_post_write_read_after_success: evidence.saw_post_write_read_after_success,
        };
    }

    if looks_like_in_progress_update(response_text) {
        return CompletionEvaluation {
            decision: CompletionDecision::Continue {
                reason: "in_progress_update".to_string(),
            },
            saw_successful_write: evidence.saw_successful_write,
            saw_successful_read: evidence.saw_successful_read,
            saw_successful_search: evidence.saw_successful_search,
            saw_successful_browser: evidence.saw_successful_browser,
            saw_post_write_read_after_success: evidence.saw_post_write_read_after_success,
        };
    }

    let explicit_blocked_outcome = looks_like_explicit_access_blocked_outcome(response_text);

    if intent.requires_write_verification
        && !evidence.saw_post_write_read_after_success
        && !explicit_blocked_outcome
    {
        return CompletionEvaluation {
            decision: CompletionDecision::Continue {
                reason: "write_task_without_post_write_verification".to_string(),
            },
            saw_successful_write: evidence.saw_successful_write,
            saw_successful_read: evidence.saw_successful_read,
            saw_successful_search: evidence.saw_successful_search,
            saw_successful_browser: evidence.saw_successful_browser,
            saw_post_write_read_after_success: evidence.saw_post_write_read_after_success,
        };
    }

    if intent.requires_search_evidence
        && !evidence.saw_successful_search
        && !explicit_blocked_outcome
    {
        return CompletionEvaluation {
            decision: CompletionDecision::Continue {
                reason: "search_task_without_search_evidence".to_string(),
            },
            saw_successful_write: evidence.saw_successful_write,
            saw_successful_read: evidence.saw_successful_read,
            saw_successful_search: evidence.saw_successful_search,
            saw_successful_browser: evidence.saw_successful_browser,
            saw_post_write_read_after_success: evidence.saw_post_write_read_after_success,
        };
    }

    if intent.requires_workspace_read_evidence
        && !evidence.saw_successful_read
        && !explicit_blocked_outcome
    {
        return CompletionEvaluation {
            decision: CompletionDecision::Continue {
                reason: "workspace_analysis_without_read_evidence".to_string(),
            },
            saw_successful_write: evidence.saw_successful_write,
            saw_successful_read: evidence.saw_successful_read,
            saw_successful_search: evidence.saw_successful_search,
            saw_successful_browser: evidence.saw_successful_browser,
            saw_post_write_read_after_success: evidence.saw_post_write_read_after_success,
        };
    }

    if intent.requires_browser_evidence
        && !evidence.saw_successful_browser
        && !explicit_blocked_outcome
    {
        return CompletionEvaluation {
            decision: CompletionDecision::Continue {
                reason: "browser_task_without_browser_evidence".to_string(),
            },
            saw_successful_write: evidence.saw_successful_write,
            saw_successful_read: evidence.saw_successful_read,
            saw_successful_search: evidence.saw_successful_search,
            saw_successful_browser: evidence.saw_successful_browser,
            saw_post_write_read_after_success: evidence.saw_post_write_read_after_success,
        };
    }

    CompletionEvaluation {
        decision: CompletionDecision::Complete,
        saw_successful_write: evidence.saw_successful_write,
        saw_successful_read: evidence.saw_successful_read,
        saw_successful_search: evidence.saw_successful_search,
        saw_successful_browser: evidence.saw_successful_browser,
        saw_post_write_read_after_success: evidence.saw_post_write_read_after_success,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExecutionRecord {
    pub tool_name: String,
    pub success: bool,
    pub output_excerpt: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolEvidenceSnapshot {
    pub saw_successful_write: bool,
    pub saw_successful_read: bool,
    pub saw_successful_search: bool,
    pub saw_successful_browser: bool,
    pub saw_post_write_read_after_success: bool,
    pub records: Vec<ToolExecutionRecord>,
}

impl ToolEvidenceSnapshot {
    pub fn evidence_tokens(&self) -> Vec<String> {
        let mut tokens = HashSet::new();
        if self.saw_successful_write {
            tokens.insert("tool_success:file_write".to_string());
        }
        if self.saw_successful_read {
            tokens.insert("tool_success:file_read".to_string());
        }
        if self.saw_successful_search {
            tokens.insert("tool_success:web_search".to_string());
        }
        if self.saw_successful_browser {
            tokens.insert("tool_success:browser".to_string());
        }
        if self.saw_post_write_read_after_success {
            tokens.insert("artifact_verified:post_write_read".to_string());
        }
        let mut out = tokens.into_iter().collect::<Vec<_>>();
        out.sort();
        out
    }
}

#[derive(Debug, Clone)]
struct ObservedToolCall {
    name: String,
    kind: ShellToolKind,
}

#[derive(Default)]
struct TaskIntent {
    requires_search_evidence: bool,
    requires_write_verification: bool,
    requires_workspace_read_evidence: bool,
    requires_browser_evidence: bool,
}

pub fn collect_tool_evidence_snapshot(history: &[ChatMessage]) -> ToolEvidenceSnapshot {
    let mut evidence = ToolEvidenceSnapshot::default();
    let mut queued_calls = VecDeque::new();
    let mut calls_by_id = HashMap::new();

    for msg in history {
        match msg.role.as_str() {
            "assistant" => {
                collect_assistant_tool_calls(&msg.content, &mut queued_calls, &mut calls_by_id);
            }
            "user" => {
                collect_prompt_tool_result_evidence(&msg.content, &mut queued_calls, &mut evidence);
            }
            "tool" => {
                collect_native_tool_result_evidence(
                    &msg.content,
                    &mut queued_calls,
                    &calls_by_id,
                    &mut evidence,
                );
            }
            _ => {}
        }
    }

    evidence
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
            let Some(name) = val.get("name").and_then(|n| n.as_str()) else {
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
    let Some(tool_calls) = val.get("tool_calls").and_then(|v| v.as_array()) else {
        return;
    };

    for call in tool_calls {
        let Some(name) = call.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let observed = if name == "shell" {
            let shell_kind = extract_shell_command_from_arguments(call.get("arguments"))
                .as_deref()
                .map(classify_shell_command)
                .unwrap_or(ShellToolKind::Other);
            ObservedToolCall {
                name: "shell".to_string(),
                kind: shell_kind,
            }
        } else {
            ObservedToolCall {
                name: name.to_string(),
                kind: classify_tool_kind(name),
            }
        };
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
            .and_then(|cmd| cmd.as_str())
            .map(ToString::to_string),
        serde_json::Value::String(raw) => serde_json::from_str::<serde_json::Value>(raw)
            .ok()
            .and_then(|parsed| {
                parsed
                    .get("command")
                    .and_then(|cmd| cmd.as_str())
                    .map(ToString::to_string)
            }),
        _ => None,
    }
}

fn try_parse_call_id(value: &serde_json::Value) -> Option<String> {
    value
        .get("id")
        .and_then(|id| id.as_str())
        .or_else(|| value.get("tool_call_id").and_then(|id| id.as_str()))
        .map(ToString::to_string)
}

fn observed_tool_call_from_name_and_args(
    tool_name: &str,
    arguments: Option<&serde_json::Value>,
) -> ObservedToolCall {
    if tool_name == "shell" {
        let shell_kind = arguments
            .and_then(|args| args.get("command"))
            .and_then(|cmd| cmd.as_str())
            .map(classify_shell_command)
            .unwrap_or(ShellToolKind::Other);
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

fn classify_tool_kind(tool_name: &str) -> ShellToolKind {
    match tool_name {
        "file_write" => ShellToolKind::WriteLike,
        "file_read" | "glob_search" | "content_search" | "pdf_read" => ShellToolKind::ReadLike,
        "web_search_tool" | "http_request" | "browser" | "browser_open" => {
            ShellToolKind::SearchLike
        }
        _ => ShellToolKind::Other,
    }
}

fn collect_prompt_tool_result_evidence(
    content: &str,
    queued_calls: &mut VecDeque<ObservedToolCall>,
    evidence: &mut ToolEvidenceSnapshot,
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
        let call = if tool_name == "shell" {
            queued_calls
                .pop_front()
                .unwrap_or_else(|| ObservedToolCall {
                    name: "shell".to_string(),
                    kind: ShellToolKind::Other,
                })
        } else {
            ObservedToolCall {
                name: tool_name.to_string(),
                kind: classify_tool_kind(tool_name),
            }
        };
        apply_tool_result_event(call, output, evidence);

        remaining = &after_body_start[close_idx + "</tool_result>".len()..];
    }
}

fn collect_native_tool_result_evidence(
    content: &str,
    queued_calls: &mut VecDeque<ObservedToolCall>,
    calls_by_id: &HashMap<String, ObservedToolCall>,
    evidence: &mut ToolEvidenceSnapshot,
) {
    let (tool_call_id, output) =
        parse_tool_message_payload(content).unwrap_or_else(|| (None, content.trim().to_string()));

    let call = tool_call_id
        .as_deref()
        .and_then(|id| calls_by_id.get(id).cloned())
        .or_else(|| queued_calls.pop_front())
        .unwrap_or_else(|| ObservedToolCall {
            name: "unknown".to_string(),
            kind: ShellToolKind::Other,
        });
    apply_tool_result_event(call, &output, evidence);
}

fn parse_tool_message_payload(content: &str) -> Option<(Option<String>, String)> {
    let val = serde_json::from_str::<serde_json::Value>(content).ok()?;
    let obj = val.as_object()?;
    let tool_call_id = obj
        .get("tool_call_id")
        .and_then(|v| v.as_str())
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

fn apply_tool_result_event(
    call: ObservedToolCall,
    output: &str,
    evidence: &mut ToolEvidenceSnapshot,
) {
    let is_success = !tool_result_output_likely_failure(output);
    if call.kind == ShellToolKind::WriteLike && is_success {
        evidence.saw_successful_write = true;
    }
    if call.kind == ShellToolKind::ReadLike && is_success && evidence.saw_successful_write {
        evidence.saw_post_write_read_after_success = true;
    }
    if call.kind == ShellToolKind::ReadLike && is_success {
        evidence.saw_successful_read = true;
    }
    if call.kind == ShellToolKind::SearchLike && is_success {
        evidence.saw_successful_search = true;
    }
    if is_success && (call.name == "browser" || call.name == "browser_open") {
        evidence.saw_successful_browser = true;
        evidence.saw_successful_search = true;
    }

    evidence.records.push(ToolExecutionRecord {
        tool_name: call.name,
        success: is_success,
        output_excerpt: to_compact_output_excerpt(output, 300),
    });
}

fn to_compact_output_excerpt(output: &str, max_chars: usize) -> String {
    let compact = output.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        return compact;
    }
    compact.chars().take(max_chars).collect::<String>() + "..."
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

    if lower.contains("curl ")
        || lower.contains("wget ")
        || lower.contains("http://")
        || lower.contains("https://")
    {
        return ShellToolKind::SearchLike;
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

fn infer_task_intent(original_request: Option<&str>) -> TaskIntent {
    let Some(request) = original_request else {
        return TaskIntent::default();
    };

    let lower = request.to_ascii_lowercase();
    let contains_zh = |hints: &[&str]| hints.iter().any(|h| request.contains(h));
    let contains_en = |hints: &[&str]| hints.iter().any(|h| lower.contains(h));

    let has_explicit_search_action = contains_zh(&[
        "搜索",
        "搜一下",
        "检索",
        "查一下",
        "查找",
        "联网",
        "新闻",
        "调研",
        "收集资料",
    ]) || contains_en(&[
        "search",
        "look up",
        "research",
        "find news",
        "latest news",
        "web",
    ]);

    let has_fetch_style_action = contains_zh(&["获取", "抓取", "拉取", "采集", "汇总", "整理一下"])
        || contains_en(&["fetch", "scrape", "collect", "gather", "pull"]);

    let has_remote_source_target = lower.contains("github")
        || contains_zh(&[
            "全网",
            "热门",
            "热榜",
            "趋势",
            "榜单",
            "开源",
            "仓库",
            "互联网",
            "网上",
        ])
        || contains_en(&[
            "github",
            "trending",
            "popular",
            "hot",
            "top repo",
            "top repositories",
            "internet",
            "online",
        ]);

    let has_search_action =
        has_explicit_search_action || (has_fetch_style_action && has_remote_source_target);

    let has_write_destination = contains_zh(&[
        "保存到",
        "存储到",
        "写入到",
        "写到",
        "工作空间",
        "文件",
        "文档",
        "路径",
    ]) || contains_en(&[
        "save to",
        "store in",
        "write to",
        "create file",
        "update file",
        "workspace",
        "path",
        ".md",
        ".txt",
    ]);

    let has_analysis_action = contains_zh(&["分析", "查看", "检查", "阅读"])
        || contains_en(&["analyze", "inspect", "review", "read"]);
    let has_workspace_target = contains_zh(&[
        "目录",
        "文件夹",
        "路径",
        "项目",
        "仓库",
        "代码库",
        "源码",
        "workspace",
        "工作空间",
        "studio",
        "src/",
        "README",
    ]) || contains_en(&[
        "directory",
        "folder",
        "path",
        "project",
        "repo",
        "repository",
        "codebase",
        "workspace",
        "studio",
        "src/",
        "readme",
    ]);

    let has_browser_intent = contains_zh(&[
        "用浏览器",
        "浏览器打开",
        "浏览器访问",
        "在浏览器中打开",
        "访问网站",
        "打开网站",
    ]) || contains_en(&[
        "open in browser",
        "open with browser",
        "visit in browser",
        "browser open",
        "browser visit",
    ]);

    TaskIntent {
        requires_search_evidence: has_search_action,
        requires_write_verification: has_write_destination,
        requires_workspace_read_evidence: has_analysis_action && has_workspace_target,
        requires_browser_evidence: has_browser_intent,
    }
}

fn looks_like_explicit_access_blocked_outcome(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();

    let zh_hints = [
        "无法访问",
        "访问被拒绝",
        "权限不足",
        "没有权限",
        "超出工作空间",
        "工作空间外",
        "符号链接",
        "allowed_roots",
        "路径不允许",
        "策略阻止",
    ];
    if zh_hints.iter().any(|h| text.contains(h)) {
        return true;
    }

    let en_hints = [
        "access denied",
        "permission denied",
        "cannot access",
        "unable to access",
        "outside workspace",
        "not allowed",
        "blocked by policy",
        "allowed_roots",
        "symlink",
    ];
    en_hints.iter().any(|h| lower.contains(h))
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
        "i'll check",
        "i will check",
        "i'll search",
        "i will search",
        "i'll gather",
        "i will gather",
        "i'll fetch",
        "i will fetch",
        "working on",
        "currently implementing",
        "我正在",
        "我来",
        "我来直接",
        "我这就",
        "我马上",
        "我会先",
        "我先去",
        "我现在去",
        "我这边先",
        "让我检查",
        "我先检查",
        "让我先查看",
        "我需要先查看",
        "我先搜",
        "我先搜索",
        "我先用网络搜索",
        "我去搜一下",
        "我去搜索",
        "正在实施",
    ];

    if progress_hints
        .iter()
        .any(|hint| lower.contains(hint) || text.contains(hint))
    {
        return true;
    }

    // Semantic "commitment update" detection:
    // if the model says it will do an action and deliver later, treat as in-progress.
    let action_verbs_zh = [
        "搜索",
        "检索",
        "查询",
        "查找",
        "获取",
        "抓取",
        "拉取",
        "调研",
        "收集",
        "汇总",
        "整理",
        "撰写",
        "分析",
        "查看",
        "检查",
        "实施",
        "修改",
        "更新",
        "执行",
        "处理",
        "调用工具",
        "联网抓取",
        "查证",
        "核实",
        "读取",
    ];
    let action_verbs_en = [
        "search",
        "look up",
        "gather",
        "collect",
        "analyze",
        "check",
        "review",
        "investigate",
        "fetch",
        "browse",
        "write",
        "draft",
        "update",
        "implement",
    ];
    let future_delivery_markers_zh = [
        "然后给你",
        "再给你",
        "稍后给你",
        "之后给你",
        "回头给你",
        "给你汇总",
        "给你速览",
        "给你结果",
        "给你结论",
        "再回复你",
        "回来告诉你",
    ];
    let future_delivery_markers_en = [
        "then i'll",
        "then i will",
        "and get back",
        "and share",
        "i'll report back",
        "i will report back",
        "i'll provide",
        "i will provide",
        "i'll send",
        "i will send",
    ];

    let has_action = action_verbs_zh.iter().any(|v| text.contains(v))
        || action_verbs_en.iter().any(|v| lower.contains(v));
    let has_future_delivery = future_delivery_markers_zh.iter().any(|m| text.contains(m))
        || future_delivery_markers_en.iter().any(|m| lower.contains(m));

    has_action && has_future_delivery
}

#[cfg(test)]
mod tests {
    use super::{
        collect_tool_evidence_snapshot, evaluate_completion, evaluate_completion_with_request,
        CompletionDecision,
    };
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

    #[test]
    fn completion_evaluator_detects_commitment_style_progress_update() {
        let history = vec![ChatMessage::user("搜一下今天的新闻")];
        let eval =
            evaluate_completion("我这就用网络搜索抓取今天的热点，并给你中文速览。", &history);
        assert_eq!(
            eval.decision,
            CompletionDecision::Continue {
                reason: "in_progress_update".to_string()
            }
        );
    }

    #[test]
    fn completion_evaluator_detects_commitment_style_progress_update_seed_dance_case() {
        let history = vec![ChatMessage::user("搜一下 seeddance 是干啥的")];
        let eval = evaluate_completion(
            "我先直接全网检索 `SeedDance`，然后给你一个“最可能指向 + 是干啥的 + 证据来源”的速览。",
            &history,
        );
        assert_eq!(
            eval.decision,
            CompletionDecision::Continue {
                reason: "in_progress_update".to_string()
            }
        );
    }

    #[test]
    fn completion_evaluator_blocks_search_task_without_search_evidence() {
        let history = vec![ChatMessage::user("搜一下今天的新闻")];
        let eval = evaluate_completion_with_request(
            "今天热点很多，我给你整理完了。",
            &history,
            Some("搜一下今天的新闻"),
        );
        assert_eq!(
            eval.decision,
            CompletionDecision::Continue {
                reason: "search_task_without_search_evidence".to_string()
            }
        );
    }

    #[test]
    fn completion_evaluator_accepts_search_task_with_web_search_tool_evidence() {
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
        let eval = evaluate_completion_with_request(
            "我已整理好今天新闻重点如下。",
            &history,
            Some("搜一下今天的新闻"),
        );
        assert_eq!(eval.decision, CompletionDecision::Complete);
    }

    #[test]
    fn completion_evaluator_blocks_workspace_analysis_without_read_evidence() {
        let history = vec![ChatMessage::user("帮我分析 studio 目录项目")];
        let eval = evaluate_completion_with_request(
            "我已经分析完 studio 项目结构，这是一个 React 项目。",
            &history,
            Some("帮我分析 studio 目录项目"),
        );
        assert_eq!(
            eval.decision,
            CompletionDecision::Continue {
                reason: "workspace_analysis_without_read_evidence".to_string()
            }
        );
    }

    #[test]
    fn completion_evaluator_allows_explicit_access_blocked_outcome_without_read_evidence() {
        let history = vec![ChatMessage::user("帮我分析 studio 目录项目")];
        let eval = evaluate_completion_with_request(
            "无法访问该路径：符号链接指向 workspace 外部，请在 [autonomy].allowed_roots 放行后重试。",
            &history,
            Some("帮我分析 studio 目录项目"),
        );
        assert_eq!(eval.decision, CompletionDecision::Complete);
    }

    #[test]
    fn completion_evaluator_detects_github_fetch_commitment_as_in_progress() {
        let history = vec![ChatMessage::user("尝试获取github上的热门skills")];
        let eval = evaluate_completion(
            "我来直接抓取 GitHub 热门仓库并汇总技能关键词，马上给你结果。",
            &history,
        );
        assert_eq!(
            eval.decision,
            CompletionDecision::Continue {
                reason: "in_progress_update".to_string()
            }
        );
    }

    #[test]
    fn completion_evaluator_blocks_github_hot_skills_without_search_evidence() {
        let history = vec![ChatMessage::user("尝试获取github上的热门skills")];
        let eval = evaluate_completion_with_request(
            "我已整理出热门 skills 清单如下。",
            &history,
            Some("尝试获取github上的热门skills"),
        );
        assert_eq!(
            eval.decision,
            CompletionDecision::Continue {
                reason: "search_task_without_search_evidence".to_string()
            }
        );
    }

    #[test]
    fn completion_evaluator_blocks_browser_task_without_browser_evidence() {
        let history = vec![ChatMessage::user("用浏览器访问zhihu.com")];
        let eval = evaluate_completion_with_request(
            "知乎网站已成功访问。",
            &history,
            Some("用浏览器访问zhihu.com"),
        );
        assert_eq!(
            eval.decision,
            CompletionDecision::Continue {
                reason: "browser_task_without_browser_evidence".to_string()
            }
        );
    }

    #[test]
    fn completion_evaluator_accepts_browser_task_with_native_tool_result_evidence() {
        let history = vec![
            ChatMessage::assistant(
                r#"{"content":null,"tool_calls":[{"id":"call_browser_1","name":"browser","arguments":"{\"action\":\"open\",\"url\":\"https://www.zhihu.com\"}"}]}"#,
            ),
            ChatMessage::tool(
                r#"{"tool_call_id":"call_browser_1","content":"{\"url\":\"https://www.zhihu.com/signin\"}"}"#,
            ),
        ];
        let eval = evaluate_completion_with_request(
            "成功访问了知乎网站。",
            &history,
            Some("用浏览器访问zhihu.com"),
        );
        assert_eq!(eval.decision, CompletionDecision::Complete);
        assert!(eval.saw_successful_browser);
    }

    #[test]
    fn evidence_snapshot_collects_native_tool_records() {
        let history = vec![
            ChatMessage::assistant(
                r#"{"content":null,"tool_calls":[{"id":"call_browser_2","name":"browser","arguments":"{\"action\":\"open\",\"url\":\"https://www.zhihu.com\"}"}]}"#,
            ),
            ChatMessage::tool(
                r#"{"tool_call_id":"call_browser_2","content":"{\"url\":\"https://www.zhihu.com/signin\"}"}"#,
            ),
        ];
        let snapshot = collect_tool_evidence_snapshot(&history);
        assert!(snapshot.saw_successful_browser);
        assert!(snapshot.saw_successful_search);
        assert_eq!(snapshot.records.len(), 1);
        assert_eq!(snapshot.records[0].tool_name, "browser");
        assert!(snapshot.records[0].success);
    }
}

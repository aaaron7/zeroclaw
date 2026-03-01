use crate::agent::task_contract::{EvidenceRequirement, TaskContract, TaskType};
use crate::config::AutonomyConfig;

pub fn compile_contract(
    request: &str,
    channel: &str,
    enabled_tools: &[String],
    autonomy: &AutonomyConfig,
) -> TaskContract {
    let _ = channel;

    if !autonomy.contract_completion_engine {
        return TaskContract::new(TaskType::Unknown);
    }

    let lower = request.to_ascii_lowercase();
    let contains_zh = |hints: &[&str]| hints.iter().any(|h| request.contains(h));
    let contains_en = |hints: &[&str]| hints.iter().any(|h| lower.contains(h));

    let is_search = contains_zh(&["搜索", "搜一下", "检索", "获取", "抓取", "新闻", "热门", "趋势"])
        || contains_en(&["search", "look up", "find", "fetch", "trending", "popular", "news"])
        || lower.contains("github");
    if is_search {
        let search_tool = choose_search_tool(enabled_tools).unwrap_or("web_search_tool");
        return TaskContract::new(TaskType::Search)
            .with_requirement(EvidenceRequirement::tool_success(search_tool));
    }

    let is_write = contains_zh(&["保存到", "存储到", "写入", "写到", "工作空间", "文档", "文件"])
        || contains_en(&["save to", "store in", "write to", "workspace", "file"]);
    if is_write {
        return TaskContract::new(TaskType::WriteArtifact)
            .with_requirement(EvidenceRequirement::tool_success("file_write"))
            .with_requirement(EvidenceRequirement::tool_success("file_read"));
    }

    let is_analysis = (contains_zh(&["分析", "查看", "检查", "阅读"])
        || contains_en(&["analyze", "inspect", "review", "read"]))
        && (contains_zh(&["目录", "文件夹", "路径", "项目", "代码"])
            || contains_en(&["directory", "folder", "path", "project", "repo", "codebase"])
            || lower.contains("studio"));
    if is_analysis {
        return TaskContract::new(TaskType::WorkspaceAnalysis)
            .with_requirement(EvidenceRequirement::tool_success("file_read"));
    }

    TaskContract::new(TaskType::Unknown)
}

fn choose_search_tool(enabled_tools: &[String]) -> Option<&str> {
    let candidates = [
        "web_search_tool",
        "http_request",
        "browser",
        "browser_open",
        "shell",
    ];
    for c in candidates {
        if enabled_tools.iter().any(|tool| tool == c) {
            return Some(c);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::compile_contract;
    use crate::agent::task_contract::TaskType;
    use crate::config::AutonomyConfig;

    #[test]
    fn compile_contract_classifies_github_hot_skills_as_search() {
        let contract = compile_contract(
            "尝试获取github上的热门skills",
            "imessage",
            &["web_search_tool".to_string(), "file_read".to_string()],
            &AutonomyConfig::default(),
        );
        assert_eq!(contract.task_type, TaskType::Search);
        assert!(
            contract
                .required_evidence
                .iter()
                .any(|r| r.id.contains("web_search_tool")),
            "search task should require successful search tool evidence"
        );
    }

    #[test]
    fn compile_contract_classifies_workspace_save_request_as_write_artifact() {
        let contract = compile_contract(
            "帮我写报告并保存到工作空间",
            "web_dashboard",
            &["file_write".to_string(), "file_read".to_string()],
            &AutonomyConfig::default(),
        );
        assert_eq!(contract.task_type, TaskType::WriteArtifact);
        assert!(
            contract
                .required_evidence
                .iter()
                .any(|r| r.id.contains("file_write")),
            "write task should require file write evidence"
        );
    }

    #[test]
    fn compile_contract_classifies_directory_analysis_as_workspace_analysis() {
        let contract = compile_contract(
            "分析 studio 目录项目",
            "imessage",
            &["file_read".to_string()],
            &AutonomyConfig::default(),
        );
        assert_eq!(contract.task_type, TaskType::WorkspaceAnalysis);
        assert!(
            contract
                .required_evidence
                .iter()
                .any(|r| r.id.contains("file_read")),
            "analysis task should require read evidence"
        );
    }

    #[test]
    fn compile_contract_defaults_to_unknown_for_ambiguous_request() {
        let contract = compile_contract(
            "继续",
            "imessage",
            &["file_read".to_string()],
            &AutonomyConfig::default(),
        );
        assert_eq!(contract.task_type, TaskType::Unknown);
        assert!(
            contract.required_evidence.is_empty(),
            "unknown request should not assume evidence shortcuts"
        );
    }
}

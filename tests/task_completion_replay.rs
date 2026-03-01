use zeroclaw::agent::task_completion::{evaluate_completion, CompletionDecision};
use zeroclaw::agent::task_contract_compiler::compile_contract;
use zeroclaw::config::AutonomyConfig;
use zeroclaw::providers::ChatMessage;

#[derive(Clone)]
enum ExpectedDecision {
    Continue,
    Blocked,
}

#[derive(Clone)]
struct ReplayFixture {
    name: &'static str,
    request: &'static str,
    response: &'static str,
    history: Vec<ChatMessage>,
    expected: ExpectedDecision,
}

fn default_enabled_tools() -> Vec<String> {
    vec![
        "web_search_tool".to_string(),
        "file_read".to_string(),
        "file_write".to_string(),
        "shell".to_string(),
    ]
}

fn assert_replay_fixture(fixture: ReplayFixture) {
    let enabled_tools = default_enabled_tools();
    let contract = compile_contract(
        fixture.request,
        "imessage",
        &enabled_tools,
        &AutonomyConfig::default(),
    );
    let eval = evaluate_completion(
        &contract,
        fixture.response,
        &fixture.history,
        fixture.request,
    );

    match fixture.expected {
        ExpectedDecision::Continue => match eval.decision {
            CompletionDecision::Continue { .. } => {}
            other => panic!("fixture {} expected continue, got {other:?}", fixture.name),
        },
        ExpectedDecision::Blocked => match eval.decision {
            CompletionDecision::Blocked { .. } => {}
            other => panic!("fixture {} expected blocked, got {other:?}", fixture.name),
        },
    }
}

#[test]
fn task_completion_replay_known_bad_cases() {
    let fixtures = vec![
        ReplayFixture {
            name: "today_news_promise_only",
            request: "搜一下今天的新闻",
            response: "我这就用网络搜索抓取今天的热点，并给你中文速览。",
            history: vec![ChatMessage::user("搜一下今天的新闻")],
            expected: ExpectedDecision::Continue,
        },
        ReplayFixture {
            name: "seeddance_direct_search_promise_only",
            request: "搜一下 seeddance 是干啥的",
            response: "我先直接全网检索 SeedDance，然后给你速览。",
            history: vec![ChatMessage::user("直接开搜")],
            expected: ExpectedDecision::Continue,
        },
        ReplayFixture {
            name: "github_hot_skills_promise_only",
            request: "尝试获取github上的热门skills",
            response: "我来直接抓取 GitHub 热门仓库并汇总技能关键词，马上给你结果。",
            history: vec![ChatMessage::user("尝试获取github上的热门skills")],
            expected: ExpectedDecision::Continue,
        },
        ReplayFixture {
            name: "studio_analysis_hallucinated_structure_without_read_evidence",
            request: "分析 studio 目录项目",
            response: "我分析了项目结构，包含 src、components、package.json 等。",
            history: vec![ChatMessage::user("分析 studio 目录项目")],
            expected: ExpectedDecision::Continue,
        },
        ReplayFixture {
            name: "write_claim_without_post_write_verification",
            request: "把报告保存到工作空间",
            response: "好的，我已经将报告保存到 中国ADHD儿童调查报告.md。",
            history: vec![
                ChatMessage::assistant(
                    r#"<tool_call>
{"name":"file_write","arguments":{"path":"中国ADHD儿童调查报告.md","content":"报告正文"}}
</tool_call>"#,
                ),
                ChatMessage::user(
                    "[Tool results]\n<tool_result name=\"file_write\">\nWritten 12 bytes\n</tool_result>",
                ),
            ],
            expected: ExpectedDecision::Continue,
        },
        ReplayFixture {
            name: "workspace_access_denied_is_blocked",
            request: "分析 studio 目录项目",
            response: "我会继续分析这个目录。",
            history: vec![
                ChatMessage::assistant(
                    r#"<tool_call>
{"name":"file_read","arguments":{"path":"studio"}}
</tool_call>"#,
                ),
                ChatMessage::user(
                    "[Tool results]\n<tool_result name=\"file_read\">\nERROR: path not allowed outside workspace\n</tool_result>",
                ),
            ],
            expected: ExpectedDecision::Blocked,
        },
    ];

    for fixture in fixtures {
        assert_replay_fixture(fixture);
    }
}

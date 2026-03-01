use crate::agent::gray_zone_verifier::{
    GrayZoneVerdict, GrayZoneVerificationRequest, GrayZoneVerifier, ProviderGrayZoneVerifier,
};
use crate::agent::loop_::run_tool_call_loop;
use crate::agent::task_completion::{evaluate_completion, CompletionDecision};
use crate::agent::task_contract::TaskType;
use crate::agent::task_contract_compiler::compile_contract;
use crate::agent::task_store::TaskStore;
use crate::agent::task_types::TaskStatus;
use crate::config::MultimodalConfig;
use crate::hooks::HookRunner;
use crate::observability::Observer;
use crate::providers::{ChatMessage, Provider};
use crate::tools::Tool;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct TaskEngineConfig {
    pub max_continuation_rounds: usize,
    pub provider_retry_limit: usize,
    pub gray_zone_verifier_enabled: bool,
    pub gray_zone_verifier_timeout_ms: u64,
}

impl Default for TaskEngineConfig {
    fn default() -> Self {
        Self {
            max_continuation_rounds: 4,
            provider_retry_limit: 2,
            gray_zone_verifier_enabled: true,
            gray_zone_verifier_timeout_ms: 1500,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaskRunOutcome {
    pub task_id: String,
    pub final_response: String,
    pub write_verified: bool,
}

pub struct TaskEngine {
    store: TaskStore,
    cfg: TaskEngineConfig,
    gray_zone_verifier: Arc<dyn GrayZoneVerifier>,
}

pub type TaskProgressReporter = Arc<dyn Fn(String) + Send + Sync>;

pub struct TaskRunRequest<'a> {
    pub channel: &'a str,
    pub sender_key: &'a str,
    pub reply_target: &'a str,
    pub original_request: &'a str,
    pub provider: &'a dyn Provider,
    pub history: &'a mut Vec<ChatMessage>,
    pub tools_registry: &'a [Box<dyn Tool>],
    pub observer: &'a dyn Observer,
    pub provider_name: &'a str,
    pub model: &'a str,
    pub temperature: f64,
    pub multimodal: &'a MultimodalConfig,
    pub max_tool_iterations: usize,
    pub cancellation_token: Option<CancellationToken>,
    pub on_delta: Option<mpsc::Sender<String>>,
    pub hooks: Option<&'a HookRunner>,
    pub excluded_tools: &'a [String],
    pub progress_reporter: Option<TaskProgressReporter>,
}

const STALLED_PROGRESS_ONLY_LIMIT: usize = 6;

#[derive(Debug)]
enum TaskEngineState {
    Running {
        round: usize,
    },
    Verifying {
        round: usize,
        response: String,
    },
    Completed {
        round: usize,
        response: String,
    },
    Blocked {
        round: usize,
        reason: String,
        remediation: String,
    },
    Failed {
        round: usize,
        reason: String,
        error: Option<String>,
    },
}

impl TaskEngine {
    pub fn new(workspace_dir: &std::path::Path, cfg: TaskEngineConfig) -> Result<Self> {
        let verifier = Arc::new(ProviderGrayZoneVerifier::new(
            cfg.gray_zone_verifier_timeout_ms,
        ));
        Self::with_verifier(workspace_dir, cfg, verifier)
    }

    pub fn with_verifier(
        workspace_dir: &std::path::Path,
        cfg: TaskEngineConfig,
        gray_zone_verifier: Arc<dyn GrayZoneVerifier>,
    ) -> Result<Self> {
        let store = TaskStore::new(workspace_dir)?;
        Ok(Self {
            store,
            cfg,
            gray_zone_verifier,
        })
    }

    pub fn store(&self) -> &TaskStore {
        &self.store
    }

    pub fn default_for_workspace(workspace_dir: &std::path::Path) -> Result<Self> {
        Self::new(workspace_dir, TaskEngineConfig::default())
    }

    pub fn create_task(
        &self,
        channel: &str,
        sender_key: &str,
        reply_target: &str,
        original_request: &str,
    ) -> Result<String> {
        let task_id = Uuid::new_v4().to_string();
        self.store.insert_task_run(
            &task_id,
            channel,
            sender_key,
            reply_target,
            original_request,
        )?;
        self.store.append_event(&task_id, "accepted", None).ok();
        Ok(task_id)
    }

    pub async fn run_task(
        mut req: TaskRunRequest<'_>,
        engine: &TaskEngine,
    ) -> Result<TaskRunOutcome> {
        let task_id = engine.create_task(
            req.channel,
            req.sender_key,
            req.reply_target,
            req.original_request,
        )?;
        engine
            .store
            .update_status(&task_id, TaskStatus::Running)
            .ok();
        engine.store.append_event(&task_id, "started", None).ok();
        emit_progress(
            &req,
            "🧠 任务已接管，进入自主执行模式。将持续汇报每一轮推进状态。",
        );

        engine.run_existing_task(&task_id, &mut req).await
    }

    pub async fn run_existing_task(
        &self,
        task_id: &str,
        req: &mut TaskRunRequest<'_>,
    ) -> Result<TaskRunOutcome> {
        let enabled_tools = enabled_tools_for_contract(req.tools_registry, req.excluded_tools);
        let contract = compile_contract(
            req.original_request,
            req.channel,
            &enabled_tools,
            &crate::config::AutonomyConfig::default(),
        );
        let _ = self.store.append_event(
            task_id,
            "contract_compiled",
            Some(&serde_json::json!({
                "task_type": format!("{:?}", contract.task_type),
                "required_evidence": contract
                    .required_evidence
                    .iter()
                    .map(|r| r.id.clone())
                    .collect::<Vec<String>>()
            })),
        );

        let mut write_verified = false;
        let mut consecutive_progress_only = 0usize;
        let mut state = TaskEngineState::Running { round: 0 };

        loop {
            state = match state {
                TaskEngineState::Running { round } => {
                    if round >= self.cfg.max_continuation_rounds {
                        TaskEngineState::Failed {
                            round,
                            reason: "max_continuation_rounds_exhausted".to_string(),
                            error: None,
                        }
                    } else {
                        emit_progress(
                            req,
                            format!(
                                "🔄 第 {}/{} 轮执行中…",
                                round + 1,
                                self.cfg.max_continuation_rounds
                            ),
                        );

                        match self.execute_single_round_with_retry(task_id, req).await {
                            Ok(response) => {
                                let _ = self.store.increment_attempt_count(task_id);
                                let _ = self.store.set_last_response(task_id, &response);
                                TaskEngineState::Verifying { round, response }
                            }
                            Err(err) => TaskEngineState::Failed {
                                round,
                                reason: "provider_error".to_string(),
                                error: Some(format!("{err:#}")),
                            },
                        }
                    }
                }
                TaskEngineState::Verifying { round, response } => {
                    emit_progress(
                        req,
                        format!(
                            "🧾 第 {} 轮输出摘要：{}",
                            round + 1,
                            summarize_round_output_for_progress(&response)
                        ),
                    );
                    let eval = evaluate_completion(
                        &contract,
                        &response,
                        req.history,
                        req.original_request,
                    );

                    if eval.saw_post_write_read_after_success && !write_verified {
                        write_verified = true;
                        let _ = self.store.upsert_artifact_verification(
                            task_id,
                            "__history_verified__",
                            None,
                            true,
                        );
                        let _ = self
                            .store
                            .append_event(task_id, "tool_write_verified", None);
                        emit_progress(req, "✅ 检测到写后校验证据（write + read/check）。");
                    }

                    match eval.decision {
                        CompletionDecision::Complete => {
                            consecutive_progress_only = 0;
                            TaskEngineState::Completed { round, response }
                        }
                        CompletionDecision::Continue {
                            reason,
                            missing_requirements,
                        } => {
                            let mut verifier_marked_done = false;
                            if self.cfg.gray_zone_verifier_enabled
                                && should_invoke_gray_zone_verifier(
                                    &reason,
                                    &missing_requirements,
                                    contract.task_type,
                                )
                            {
                                let verifier_request = GrayZoneVerificationRequest {
                                    provider: req.provider,
                                    model: req.model,
                                    original_request: req.original_request,
                                    model_response: &response,
                                    continue_reason: &reason,
                                    missing_requirements: &missing_requirements,
                                };
                                match self.gray_zone_verifier.verify(verifier_request).await {
                                    Ok(GrayZoneVerdict {
                                        done: true,
                                        reason: verifier_reason,
                                    }) if gray_zone_completion_allowed(
                                        contract.task_type,
                                        &missing_requirements,
                                    ) =>
                                    {
                                        let _ = self.store.append_event(
                                            task_id,
                                            "gray_zone_verifier",
                                            Some(&serde_json::json!({
                                                "result":"done",
                                                "reason": verifier_reason,
                                                "round": round + 1
                                            })),
                                        );
                                        consecutive_progress_only = 0;
                                        verifier_marked_done = true;
                                    }
                                    Ok(verdict) => {
                                        let _ = self.store.append_event(
                                            task_id,
                                            "gray_zone_verifier",
                                            Some(&serde_json::json!({
                                                "result":"continue",
                                                "done": verdict.done,
                                                "reason": verdict.reason,
                                                "round": round + 1
                                            })),
                                        );
                                    }
                                    Err(err) => {
                                        let _ = self.store.append_event(
                                            task_id,
                                            "gray_zone_verifier_error",
                                            Some(&serde_json::json!({
                                                "error": format!("{err:#}"),
                                                "round": round + 1
                                            })),
                                        );
                                    }
                                }
                            }

                            if verifier_marked_done {
                                TaskEngineState::Completed { round, response }
                            } else {
                                let _ = self.store.append_event(
                                    task_id,
                                    "continue",
                                    Some(&serde_json::json!({
                                        "reason": reason,
                                        "round": round + 1,
                                        "missing_requirements": missing_requirements
                                    })),
                                );
                                emit_progress(
                                    req,
                                    format!(
                                        "⏳ 第 {} 轮尚未完成（{}），继续推进…",
                                        round + 1,
                                        explain_continue_reason(&reason)
                                    ),
                                );

                                consecutive_progress_only += 1;
                                if consecutive_progress_only >= STALLED_PROGRESS_ONLY_LIMIT {
                                    TaskEngineState::Failed {
                                        round,
                                        reason: "stalled_loop".to_string(),
                                        error: None,
                                    }
                                } else {
                                    req.history.push(ChatMessage::user(
                                        "[Task Engine]\n任务尚未完成。请继续执行必要的工具操作并在有可验证结果后再给最终答复。不要仅汇报进行中状态。",
                                    ));
                                    TaskEngineState::Running { round: round + 1 }
                                }
                            }
                        }
                        CompletionDecision::Blocked {
                            reason,
                            remediation,
                        } => {
                            consecutive_progress_only = 0;
                            TaskEngineState::Blocked {
                                round,
                                reason,
                                remediation,
                            }
                        }
                        CompletionDecision::Failed { reason } => TaskEngineState::Failed {
                            round,
                            reason,
                            error: None,
                        },
                    }
                }
                TaskEngineState::Completed { round, response } => {
                    let _ = self.store.update_status(task_id, TaskStatus::Completed);
                    let _ = self.store.append_event(
                        task_id,
                        "completed",
                        Some(&serde_json::json!({"round": round + 1})),
                    );
                    emit_progress(req, format!("✅ 任务完成（第 {} 轮）。", round + 1));
                    return Ok(TaskRunOutcome {
                        task_id: task_id.to_string(),
                        final_response: response,
                        write_verified,
                    });
                }
                TaskEngineState::Blocked {
                    round,
                    reason,
                    remediation,
                } => {
                    let _ = self.store.update_status(task_id, TaskStatus::Blocked);
                    let _ = self.store.append_event(
                        task_id,
                        "blocked",
                        Some(&serde_json::json!({
                            "reason": reason,
                            "remediation": remediation,
                            "round": round + 1
                        })),
                    );
                    emit_progress(req, "⛔ 任务被阻塞（缺少必要权限或访问边界不满足）。");
                    let blocked_summary =
                        format!("任务已阻塞：{}\n建议处理：{}", reason, remediation);
                    return Ok(TaskRunOutcome {
                        task_id: task_id.to_string(),
                        final_response: blocked_summary,
                        write_verified,
                    });
                }
                TaskEngineState::Failed {
                    round,
                    reason,
                    error,
                } => {
                    let _ = self.store.update_status(task_id, TaskStatus::Failed);
                    match reason.as_str() {
                        "provider_error" => {
                            let _ = self.store.append_event(
                                task_id,
                                "failed",
                                Some(&serde_json::json!({
                                    "reason":"provider_error",
                                    "error": error.clone().unwrap_or_default(),
                                    "round": round + 1
                                })),
                            );
                            emit_progress(req, "❌ 执行失败（provider/transport 错误）。");
                            if let Some(err) = error {
                                anyhow::bail!("{err}");
                            }
                            anyhow::bail!("Task failed with provider error");
                        }
                        "max_continuation_rounds_exhausted" => {
                            let _ = self.store.append_event(
                                task_id,
                                "failed",
                                Some(&serde_json::json!({
                                    "reason":"max_continuation_rounds_exhausted",
                                    "max_rounds": self.cfg.max_continuation_rounds
                                })),
                            );
                            emit_progress(
                                req,
                                format!(
                                    "❌ 已达到最大轮数 {}，任务失败。",
                                    self.cfg.max_continuation_rounds
                                ),
                            );
                            anyhow::bail!(
                                "Task exceeded max continuation rounds ({})",
                                self.cfg.max_continuation_rounds
                            );
                        }
                        "stalled_loop" => {
                            let _ = self.store.append_event(
                                task_id,
                                "failed",
                                Some(&serde_json::json!({"reason":"stalled_loop","round": round + 1})),
                            );
                            emit_progress(req, "❌ 连续进度汇报未产出有效结果，任务失败。");
                            anyhow::bail!("Task stalled in repeated progress-only replies");
                        }
                        _ => {
                            let _ = self.store.append_event(
                                task_id,
                                "failed",
                                Some(&serde_json::json!({"reason":reason,"round": round + 1})),
                            );
                            emit_progress(req, "❌ 任务验证失败。");
                            anyhow::bail!("Task failed verification: {reason}");
                        }
                    }
                }
            };
        }
    }

    async fn execute_single_round_with_retry(
        &self,
        task_id: &str,
        req: &mut TaskRunRequest<'_>,
    ) -> Result<String> {
        let mut last_error: Option<anyhow::Error> = None;
        for attempt in 0..=self.cfg.provider_retry_limit {
            let result = run_tool_call_loop(
                req.provider,
                req.history,
                req.tools_registry,
                req.observer,
                req.provider_name,
                req.model,
                req.temperature,
                true,
                None,
                req.channel,
                req.multimodal,
                req.max_tool_iterations,
                req.cancellation_token.clone(),
                req.on_delta.clone(),
                req.hooks,
                req.excluded_tools,
            )
            .await;

            match result {
                Ok(text) => return Ok(text),
                Err(err) => {
                    let retryable = is_retryable_provider_transport_error(&err);
                    if retryable && attempt < self.cfg.provider_retry_limit {
                        let _ = self.store.increment_provider_retry_count(task_id);
                        let _ = self.store.append_event(
                            task_id,
                            "provider_retry",
                            Some(&serde_json::json!({
                                "attempt": attempt + 1,
                                "error": format!("{err:#}")
                            })),
                        );
                        emit_progress(
                            req,
                            format!(
                                "🌐 Provider 连接异常，重试 {}/{} …",
                                attempt + 1,
                                self.cfg.provider_retry_limit
                            ),
                        );
                        last_error = Some(err);
                        continue;
                    }
                    return Err(err);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Unknown task round error")))
    }
}

fn is_retryable_provider_transport_error(err: &anyhow::Error) -> bool {
    let lower = format!("{err:#}").to_ascii_lowercase();
    lower.contains("transport error")
        || lower.contains("error sending request for url")
        || lower.contains("connection reset")
        || lower.contains("connection refused")
        || lower.contains("timed out")
}

fn enabled_tools_for_contract(
    tools_registry: &[Box<dyn Tool>],
    excluded_tools: &[String],
) -> Vec<String> {
    tools_registry
        .iter()
        .map(|tool| tool.name().to_string())
        .filter(|name| !excluded_tools.iter().any(|excluded| excluded == name))
        .collect()
}

fn should_invoke_gray_zone_verifier(
    reason: &str,
    missing_requirements: &[String],
    task_type: TaskType,
) -> bool {
    task_type == TaskType::Unknown
        && missing_requirements.is_empty()
        && reason == "unknown_contract_non_terminal_update"
}

fn gray_zone_completion_allowed(task_type: TaskType, missing_requirements: &[String]) -> bool {
    task_type == TaskType::Unknown && missing_requirements.is_empty()
}

fn emit_progress(req: &TaskRunRequest<'_>, message: impl Into<String>) {
    if let Some(reporter) = req.progress_reporter.as_ref() {
        reporter(message.into());
    }
}

fn summarize_round_output_for_progress(response: &str) -> String {
    let normalized = response.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return "（空响应）".to_string();
    }
    normalized
}

fn explain_continue_reason(reason: &str) -> &str {
    match reason {
        "missing_required_evidence" => "缺少合同要求的工具执行证据",
        "unknown_contract_non_terminal_update" => "未知任务类型且回复仍处于进行中",
        "guardrail_notice" => "触发 guardrail 继续执行",
        _ => reason,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        is_retryable_provider_transport_error, TaskEngine, TaskEngineConfig, TaskRunRequest,
    };
    use crate::agent::gray_zone_verifier::{
        GrayZoneVerdict, GrayZoneVerificationRequest, GrayZoneVerifier,
    };
    use crate::observability::NoopObserver;
    use crate::providers::{ChatMessage, Provider};
    use crate::tools::Tool;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    struct ScriptedProvider {
        responses: Mutex<Vec<anyhow::Result<String>>>,
    }

    impl ScriptedProvider {
        fn new(responses: Vec<anyhow::Result<String>>) -> Self {
            Self {
                responses: Mutex::new(responses),
            }
        }
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            let mut guard = self.responses.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_empty() {
                return Ok("done".to_string());
            }
            guard.remove(0)
        }
    }

    struct ScriptedGrayZoneVerifier {
        results: Mutex<Vec<anyhow::Result<GrayZoneVerdict>>>,
        call_count: AtomicUsize,
    }

    impl ScriptedGrayZoneVerifier {
        fn new(results: Vec<anyhow::Result<GrayZoneVerdict>>) -> Self {
            Self {
                results: Mutex::new(results),
                call_count: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.call_count.load(Ordering::Relaxed)
        }
    }

    #[async_trait]
    impl GrayZoneVerifier for ScriptedGrayZoneVerifier {
        async fn verify(
            &self,
            _request: GrayZoneVerificationRequest<'_>,
        ) -> anyhow::Result<GrayZoneVerdict> {
            self.call_count.fetch_add(1, Ordering::Relaxed);
            let mut guard = self.results.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_empty() {
                return Ok(GrayZoneVerdict {
                    done: false,
                    reason: "no_scripted_result".to_string(),
                });
            }
            guard.remove(0)
        }
    }

    #[test]
    fn provider_transport_error_is_classified_retryable() {
        let err = anyhow::anyhow!(
            "Custom native chat transport error: error sending request for url (https://x)"
        );
        assert!(is_retryable_provider_transport_error(&err));
    }

    #[tokio::test]
    async fn run_task_continues_on_progress_reply_without_user_followup() {
        let tmp = TempDir::new().expect("tempdir");
        let engine = TaskEngine::new(
            tmp.path(),
            TaskEngineConfig {
                max_continuation_rounds: 4,
                provider_retry_limit: 0,
                gray_zone_verifier_enabled: false,
                gray_zone_verifier_timeout_ms: 1500,
            },
        )
        .expect("task engine");
        let provider = ScriptedProvider::new(vec![
            Ok("我正在检查当前文件状态。".to_string()),
            Ok("任务已完成。".to_string()),
        ]);
        let observer = NoopObserver;
        let mut history = vec![
            ChatMessage::system("system"),
            ChatMessage::user("请继续处理这个任务"),
        ];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let req = TaskRunRequest {
            channel: "imessage",
            sender_key: "sender-a",
            reply_target: "sender-a",
            original_request: "请继续处理这个任务",
            provider: &provider,
            history: &mut history,
            tools_registry: &tools_registry,
            observer: &observer,
            provider_name: "test-provider",
            model: "test-model",
            temperature: 0.0,
            multimodal: &crate::config::MultimodalConfig::default(),
            max_tool_iterations: 5,
            cancellation_token: None,
            on_delta: None,
            hooks: None,
            excluded_tools: &[],
            progress_reporter: None,
        };

        let outcome = TaskEngine::run_task(req, &engine)
            .await
            .expect("task should complete");
        assert_eq!(outcome.final_response, "任务已完成。");

        let row = engine
            .store()
            .get_task_run(&outcome.task_id)
            .expect("get task")
            .expect("task exists");
        assert_eq!(row.status.as_str(), "completed");
        assert!(row.attempt_count >= 2);
    }

    #[tokio::test]
    async fn run_task_retries_transport_error_then_succeeds() {
        let tmp = TempDir::new().expect("tempdir");
        let engine = TaskEngine::new(
            tmp.path(),
            TaskEngineConfig {
                max_continuation_rounds: 2,
                provider_retry_limit: 1,
                gray_zone_verifier_enabled: false,
                gray_zone_verifier_timeout_ms: 1500,
            },
        )
        .expect("task engine");
        let provider = ScriptedProvider::new(vec![
            Err(anyhow::anyhow!(
                "Custom native chat transport error: error sending request for url (https://x)"
            )),
            Ok("done".to_string()),
        ]);
        let observer = NoopObserver;
        let mut history = vec![ChatMessage::system("sys"), ChatMessage::user("hi")];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let req = TaskRunRequest {
            channel: "imessage",
            sender_key: "sender-a",
            reply_target: "sender-a",
            original_request: "hi",
            provider: &provider,
            history: &mut history,
            tools_registry: &tools_registry,
            observer: &observer,
            provider_name: "test-provider",
            model: "test-model",
            temperature: 0.0,
            multimodal: &crate::config::MultimodalConfig::default(),
            max_tool_iterations: 5,
            cancellation_token: None,
            on_delta: None,
            hooks: None,
            excluded_tools: &[],
            progress_reporter: None,
        };

        let outcome = TaskEngine::run_task(req, &engine)
            .await
            .expect("task should complete after retry");
        assert_eq!(outcome.final_response, "done");

        let row = engine
            .store()
            .get_task_run(&outcome.task_id)
            .expect("get task")
            .expect("task exists");
        assert!(row.provider_retry_count >= 1);
        assert_eq!(row.status.as_str(), "completed");
    }

    #[tokio::test]
    async fn run_task_invokes_gray_zone_verifier_once_for_unknown_progress_update() {
        let tmp = TempDir::new().expect("tempdir");
        let verifier = Arc::new(ScriptedGrayZoneVerifier::new(vec![Ok(GrayZoneVerdict {
            done: false,
            reason: "need_more_work".to_string(),
        })]));
        let engine = TaskEngine::with_verifier(
            tmp.path(),
            TaskEngineConfig {
                max_continuation_rounds: 4,
                provider_retry_limit: 0,
                gray_zone_verifier_enabled: true,
                gray_zone_verifier_timeout_ms: 1500,
            },
            verifier.clone(),
        )
        .expect("task engine");
        let provider = ScriptedProvider::new(vec![
            Ok("我正在检查当前文件状态。".to_string()),
            Ok("任务已完成。".to_string()),
        ]);
        let observer = NoopObserver;
        let mut history = vec![
            ChatMessage::system("system"),
            ChatMessage::user("请继续处理这个任务"),
        ];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let req = TaskRunRequest {
            channel: "imessage",
            sender_key: "sender-a",
            reply_target: "sender-a",
            original_request: "请继续处理这个任务",
            provider: &provider,
            history: &mut history,
            tools_registry: &tools_registry,
            observer: &observer,
            provider_name: "test-provider",
            model: "test-model",
            temperature: 0.0,
            multimodal: &crate::config::MultimodalConfig::default(),
            max_tool_iterations: 5,
            cancellation_token: None,
            on_delta: None,
            hooks: None,
            excluded_tools: &[],
            progress_reporter: None,
        };

        let outcome = TaskEngine::run_task(req, &engine)
            .await
            .expect("task should complete");
        assert_eq!(outcome.final_response, "任务已完成。");
        assert_eq!(verifier.calls(), 1);
    }

    #[tokio::test]
    async fn run_task_gray_zone_verifier_done_true_completes_in_same_round() {
        let tmp = TempDir::new().expect("tempdir");
        let verifier = Arc::new(ScriptedGrayZoneVerifier::new(vec![Ok(GrayZoneVerdict {
            done: true,
            reason: "verified_done".to_string(),
        })]));
        let engine = TaskEngine::with_verifier(
            tmp.path(),
            TaskEngineConfig {
                max_continuation_rounds: 4,
                provider_retry_limit: 0,
                gray_zone_verifier_enabled: true,
                gray_zone_verifier_timeout_ms: 1500,
            },
            verifier.clone(),
        )
        .expect("task engine");
        let provider = ScriptedProvider::new(vec![Ok("我正在检查当前文件状态。".to_string())]);
        let observer = NoopObserver;
        let mut history = vec![
            ChatMessage::system("system"),
            ChatMessage::user("请继续处理这个任务"),
        ];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let req = TaskRunRequest {
            channel: "imessage",
            sender_key: "sender-a",
            reply_target: "sender-a",
            original_request: "请继续处理这个任务",
            provider: &provider,
            history: &mut history,
            tools_registry: &tools_registry,
            observer: &observer,
            provider_name: "test-provider",
            model: "test-model",
            temperature: 0.0,
            multimodal: &crate::config::MultimodalConfig::default(),
            max_tool_iterations: 5,
            cancellation_token: None,
            on_delta: None,
            hooks: None,
            excluded_tools: &[],
            progress_reporter: None,
        };

        let outcome = TaskEngine::run_task(req, &engine)
            .await
            .expect("task should complete");
        assert_eq!(outcome.final_response, "我正在检查当前文件状态。");
        assert_eq!(verifier.calls(), 1);
        let row = engine
            .store()
            .get_task_run(&outcome.task_id)
            .expect("get task")
            .expect("task exists");
        assert_eq!(row.status.as_str(), "completed");
        assert_eq!(row.attempt_count, 1);
    }

    #[tokio::test]
    async fn run_task_gray_zone_verifier_error_falls_back_to_conservative_continue() {
        let tmp = TempDir::new().expect("tempdir");
        let verifier = Arc::new(ScriptedGrayZoneVerifier::new(vec![Err(anyhow::anyhow!(
            "gray-zone verifier timed out"
        ))]));
        let engine = TaskEngine::with_verifier(
            tmp.path(),
            TaskEngineConfig {
                max_continuation_rounds: 4,
                provider_retry_limit: 0,
                gray_zone_verifier_enabled: true,
                gray_zone_verifier_timeout_ms: 1500,
            },
            verifier.clone(),
        )
        .expect("task engine");
        let provider = ScriptedProvider::new(vec![
            Ok("我正在检查当前文件状态。".to_string()),
            Ok("任务已完成。".to_string()),
        ]);
        let observer = NoopObserver;
        let mut history = vec![
            ChatMessage::system("system"),
            ChatMessage::user("请继续处理这个任务"),
        ];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let req = TaskRunRequest {
            channel: "imessage",
            sender_key: "sender-a",
            reply_target: "sender-a",
            original_request: "请继续处理这个任务",
            provider: &provider,
            history: &mut history,
            tools_registry: &tools_registry,
            observer: &observer,
            provider_name: "test-provider",
            model: "test-model",
            temperature: 0.0,
            multimodal: &crate::config::MultimodalConfig::default(),
            max_tool_iterations: 5,
            cancellation_token: None,
            on_delta: None,
            hooks: None,
            excluded_tools: &[],
            progress_reporter: None,
        };

        let outcome = TaskEngine::run_task(req, &engine)
            .await
            .expect("task should complete");
        assert_eq!(outcome.final_response, "任务已完成。");
        assert_eq!(verifier.calls(), 1);
        let row = engine
            .store()
            .get_task_run(&outcome.task_id)
            .expect("get task")
            .expect("task exists");
        assert!(row.attempt_count >= 2);
        assert_eq!(row.status.as_str(), "completed");
    }

    #[tokio::test]
    async fn task_engine_state_machine_running_verifying_completed_transition() {
        let tmp = TempDir::new().expect("tempdir");
        let engine = TaskEngine::new(
            tmp.path(),
            TaskEngineConfig {
                max_continuation_rounds: 4,
                provider_retry_limit: 0,
                gray_zone_verifier_enabled: false,
                gray_zone_verifier_timeout_ms: 1500,
            },
        )
        .expect("task engine");
        let provider = ScriptedProvider::new(vec![
            Ok("我正在检查当前文件状态。".to_string()),
            Ok("任务已完成。".to_string()),
        ]);
        let observer = NoopObserver;
        let mut history = vec![
            ChatMessage::system("system"),
            ChatMessage::user("请继续处理这个任务"),
        ];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let req = TaskRunRequest {
            channel: "imessage",
            sender_key: "sender-a",
            reply_target: "sender-a",
            original_request: "请继续处理这个任务",
            provider: &provider,
            history: &mut history,
            tools_registry: &tools_registry,
            observer: &observer,
            provider_name: "test-provider",
            model: "test-model",
            temperature: 0.0,
            multimodal: &crate::config::MultimodalConfig::default(),
            max_tool_iterations: 5,
            cancellation_token: None,
            on_delta: None,
            hooks: None,
            excluded_tools: &[],
            progress_reporter: None,
        };

        let outcome = TaskEngine::run_task(req, &engine)
            .await
            .expect("task should complete");
        assert_eq!(outcome.final_response, "任务已完成。");

        let events = engine
            .store()
            .list_events(&outcome.task_id)
            .expect("list events");
        let event_types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert!(event_types.contains(&"continue"));
        assert!(event_types.contains(&"completed"));
    }

    #[tokio::test]
    async fn task_engine_state_machine_running_verifying_blocked_transition() {
        let tmp = TempDir::new().expect("tempdir");
        let engine = TaskEngine::new(
            tmp.path(),
            TaskEngineConfig {
                max_continuation_rounds: 4,
                provider_retry_limit: 0,
                gray_zone_verifier_enabled: false,
                gray_zone_verifier_timeout_ms: 1500,
            },
        )
        .expect("task engine");
        let provider = ScriptedProvider::new(vec![Ok("我会继续分析这个目录。".to_string())]);
        let observer = NoopObserver;
        let mut history = vec![
            ChatMessage::system("system"),
            ChatMessage::user("分析 studio 目录项目"),
            ChatMessage::assistant(
                r#"<tool_call>
{"name":"file_read","arguments":{"path":"studio"}}
</tool_call>"#,
            ),
            ChatMessage::user(
                "[Tool results]\n<tool_result name=\"file_read\">\nERROR: path not allowed outside workspace\n</tool_result>",
            ),
        ];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let req = TaskRunRequest {
            channel: "imessage",
            sender_key: "sender-a",
            reply_target: "sender-a",
            original_request: "分析 studio 目录项目",
            provider: &provider,
            history: &mut history,
            tools_registry: &tools_registry,
            observer: &observer,
            provider_name: "test-provider",
            model: "test-model",
            temperature: 0.0,
            multimodal: &crate::config::MultimodalConfig::default(),
            max_tool_iterations: 5,
            cancellation_token: None,
            on_delta: None,
            hooks: None,
            excluded_tools: &[],
            progress_reporter: None,
        };

        let outcome = TaskEngine::run_task(req, &engine)
            .await
            .expect("task should be blocked");
        assert!(outcome.final_response.contains("任务已阻塞"));

        let row = engine
            .store()
            .get_task_run(&outcome.task_id)
            .expect("get task")
            .expect("task exists");
        assert_eq!(row.status.as_str(), "blocked");
    }

    #[tokio::test]
    async fn task_engine_state_machine_running_verifying_continue_running_transition() {
        let tmp = TempDir::new().expect("tempdir");
        let engine = TaskEngine::new(
            tmp.path(),
            TaskEngineConfig {
                max_continuation_rounds: 4,
                provider_retry_limit: 0,
                gray_zone_verifier_enabled: false,
                gray_zone_verifier_timeout_ms: 1500,
            },
        )
        .expect("task engine");
        let provider = ScriptedProvider::new(vec![
            Ok("我正在检查当前文件状态。".to_string()),
            Ok("我正在继续处理，请稍等。".to_string()),
            Ok("任务已完成。".to_string()),
        ]);
        let observer = NoopObserver;
        let mut history = vec![
            ChatMessage::system("system"),
            ChatMessage::user("请继续处理这个任务"),
        ];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let req = TaskRunRequest {
            channel: "imessage",
            sender_key: "sender-a",
            reply_target: "sender-a",
            original_request: "请继续处理这个任务",
            provider: &provider,
            history: &mut history,
            tools_registry: &tools_registry,
            observer: &observer,
            provider_name: "test-provider",
            model: "test-model",
            temperature: 0.0,
            multimodal: &crate::config::MultimodalConfig::default(),
            max_tool_iterations: 5,
            cancellation_token: None,
            on_delta: None,
            hooks: None,
            excluded_tools: &[],
            progress_reporter: None,
        };

        let outcome = TaskEngine::run_task(req, &engine)
            .await
            .expect("task should complete");
        assert_eq!(outcome.final_response, "任务已完成。");

        let row = engine
            .store()
            .get_task_run(&outcome.task_id)
            .expect("get task")
            .expect("task exists");
        assert!(row.attempt_count >= 3);
    }

    #[tokio::test]
    async fn task_engine_state_machine_detects_stalled_loop_after_six_progress_only_rounds() {
        let tmp = TempDir::new().expect("tempdir");
        let engine = TaskEngine::new(
            tmp.path(),
            TaskEngineConfig {
                max_continuation_rounds: 8,
                provider_retry_limit: 0,
                gray_zone_verifier_enabled: false,
                gray_zone_verifier_timeout_ms: 1500,
            },
        )
        .expect("task engine");
        let provider = ScriptedProvider::new(vec![
            Ok("我正在检查当前文件状态。".to_string()),
            Ok("我正在继续处理，请稍等。".to_string()),
            Ok("我会继续处理，请稍等。".to_string()),
            Ok("我正在处理更多细节，请稍等。".to_string()),
            Ok("我正在继续推进，请稍等。".to_string()),
            Ok("我会继续处理，很快给你结果。".to_string()),
            Ok("我正在收敛结果，请稍后。".to_string()),
        ]);
        let observer = NoopObserver;
        let mut history = vec![
            ChatMessage::system("system"),
            ChatMessage::user("请继续处理这个任务"),
        ];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let req = TaskRunRequest {
            channel: "imessage",
            sender_key: "sender-a",
            reply_target: "sender-a",
            original_request: "请继续处理这个任务",
            provider: &provider,
            history: &mut history,
            tools_registry: &tools_registry,
            observer: &observer,
            provider_name: "test-provider",
            model: "test-model",
            temperature: 0.0,
            multimodal: &crate::config::MultimodalConfig::default(),
            max_tool_iterations: 5,
            cancellation_token: None,
            on_delta: None,
            hooks: None,
            excluded_tools: &[],
            progress_reporter: None,
        };

        let err = TaskEngine::run_task(req, &engine)
            .await
            .expect_err("task should fail due to stalled loop");
        assert!(format!("{err:#}").contains("stalled"));
    }

    #[test]
    fn summarize_round_output_for_progress_keeps_full_content_and_normalizes_whitespace() {
        let raw = format!("  第一行  \n 第二行   {}\n\n", "A".repeat(300));
        let preview = super::summarize_round_output_for_progress(&raw);
        assert!(preview.contains("第一行 第二行"));
        assert!(preview.contains(&"A".repeat(300)));
        assert!(!preview.ends_with("..."));
    }
}

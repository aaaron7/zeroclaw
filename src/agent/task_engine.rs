use crate::agent::loop_::run_tool_call_loop;
use crate::agent::task_completion::{evaluate_completion, CompletionDecision};
use crate::agent::task_store::TaskStore;
use crate::agent::task_types::TaskStatus;
use crate::config::MultimodalConfig;
use crate::hooks::HookRunner;
use crate::observability::Observer;
use crate::providers::{ChatMessage, Provider};
use crate::tools::Tool;
use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct TaskEngineConfig {
    pub max_continuation_rounds: usize,
    pub provider_retry_limit: usize,
}

impl Default for TaskEngineConfig {
    fn default() -> Self {
        Self {
            max_continuation_rounds: 4,
            provider_retry_limit: 2,
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
}

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
}

impl TaskEngine {
    pub fn new(workspace_dir: &std::path::Path, cfg: TaskEngineConfig) -> Result<Self> {
        let store = TaskStore::new(workspace_dir)?;
        Ok(Self { store, cfg })
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

        engine.run_existing_task(&task_id, &mut req).await
    }

    pub async fn run_existing_task(
        &self,
        task_id: &str,
        req: &mut TaskRunRequest<'_>,
    ) -> Result<TaskRunOutcome> {
        let mut write_verified = false;
        let mut consecutive_progress_only = 0usize;

        for round in 0..self.cfg.max_continuation_rounds {
            let response = self
                .execute_single_round_with_retry(task_id, req)
                .await
                .map_err(|err| {
                    let msg = format!("{err:#}");
                    let _ = self.store.update_status(task_id, TaskStatus::Failed);
                    let _ = self.store.append_event(
                        task_id,
                        "failed",
                        Some(&serde_json::json!({"reason":"provider_error","error":msg})),
                    );
                    err
                })?;

            let _ = self.store.increment_attempt_count(task_id);
            let _ = self.store.set_last_response(task_id, &response);
            let eval = evaluate_completion(&response, req.history);

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
            }

            match eval.decision {
                CompletionDecision::Complete => {
                    let _ = self.store.update_status(task_id, TaskStatus::Completed);
                    let _ = self.store.append_event(
                        task_id,
                        "completed",
                        Some(&serde_json::json!({"round": round + 1})),
                    );
                    return Ok(TaskRunOutcome {
                        task_id: task_id.to_string(),
                        final_response: response,
                        write_verified,
                    });
                }
                CompletionDecision::Continue { reason } => {
                    let _ = self.store.append_event(
                        task_id,
                        "continue",
                        Some(&serde_json::json!({"reason": reason, "round": round + 1})),
                    );
                    consecutive_progress_only += 1;
                    if consecutive_progress_only >= 3 {
                        let msg = "Task stalled in repeated progress-only replies".to_string();
                        let _ = self.store.update_status(task_id, TaskStatus::Failed);
                        let _ = self.store.append_event(
                            task_id,
                            "failed",
                            Some(&serde_json::json!({"reason":"stalled_loop"})),
                        );
                        anyhow::bail!("{msg}");
                    }
                    req.history.push(ChatMessage::user(
                        "[Task Engine]\n任务尚未完成。请继续执行必要的工具操作并在有可验证结果后再给最终答复。不要仅汇报进行中状态。",
                    ));
                }
            }
        }

        let _ = self.store.update_status(task_id, TaskStatus::Failed);
        let _ = self.store.append_event(
            task_id,
            "failed",
            Some(&serde_json::json!({"reason":"max_continuation_rounds_exhausted"})),
        );
        anyhow::bail!(
            "Task exceeded max continuation rounds ({})",
            self.cfg.max_continuation_rounds
        )
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

#[cfg(test)]
mod tests {
    use super::{
        is_retryable_provider_transport_error, TaskEngine, TaskEngineConfig, TaskRunRequest,
    };
    use crate::observability::NoopObserver;
    use crate::providers::{ChatMessage, Provider};
    use crate::tools::Tool;
    use async_trait::async_trait;
    use std::sync::Mutex;
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
}

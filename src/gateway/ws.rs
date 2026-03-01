//! WebSocket agent chat handler.
//!
//! Protocol:
//! ```text
//! Client -> Server: {"type":"message","content":"Hello"}
//! Server -> Client: {"type":"chunk","content":"Hi! "}
//! Server -> Client: {"type":"tool_call","name":"shell","args":{...}}
//! Server -> Client: {"type":"tool_result","name":"shell","output":"..."}
//! Server -> Client: {"type":"done","full_response":"..."}
//! ```

use super::AppState;
use crate::agent::loop_::{build_tool_instructions, run_tool_call_loop, DRAFT_CLEAR_SENTINEL};
use crate::agent::task_engine::{TaskEngine, TaskProgressReporter, TaskRunRequest};
use crate::providers::ChatMessage;
use axum::{
    extract::{
        ws::{Message, WebSocket},
        Query, State, WebSocketUpgrade,
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct WsQuery {
    pub token: Option<String>,
}

/// GET /ws/chat — WebSocket upgrade for agent chat
pub async fn handle_ws_chat(
    State(state): State<AppState>,
    Query(params): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    // Auth via query param (browser WebSocket limitation)
    if state.pairing.require_pairing() {
        let token = params.token.as_deref().unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                "Unauthorized — provide ?token=<bearer_token>",
            )
                .into_response();
        }
    }

    ws.on_upgrade(move |socket| handle_socket(socket, state))
        .into_response()
}

fn build_ws_system_prompt(state: &AppState) -> String {
    let config_guard = state.config.lock();
    let skills = crate::skills::load_skills_with_config(&config_guard.workspace_dir, &config_guard);
    let tool_descs: Vec<(&str, &str)> = state
        .runtime_tools
        .iter()
        .map(|tool| (tool.name(), tool.description()))
        .collect();
    let bootstrap_max_chars = if config_guard.agent.compact_context {
        Some(6000)
    } else {
        None
    };

    let mut prompt = crate::channels::build_system_prompt_with_mode(
        &config_guard.workspace_dir,
        &state.model,
        &tool_descs,
        &skills,
        Some(&config_guard.identity),
        bootstrap_max_chars,
        state.provider.supports_native_tools(),
        config_guard.skills.prompt_injection_mode,
    );
    if !state.provider.supports_native_tools() {
        prompt.push_str(&build_tool_instructions(state.runtime_tools.as_ref()));
    }
    prompt
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<serde_json::Value>();
    let writer = tokio::spawn(async move {
        while let Some(payload) = outbound_rx.recv().await {
            if ws_sender
                .send(Message::Text(payload.to_string().into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let mut history = vec![ChatMessage::system(build_ws_system_prompt(&state))];
    let sender_key = format!("web-dashboard:{}", Uuid::new_v4());

    while let Some(msg) = ws_receiver.next().await {
        let msg = match msg {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(_)) => break,
            Err(_) => break,
            _ => continue,
        };

        // Parse incoming message
        let parsed: serde_json::Value = match serde_json::from_str(&msg) {
            Ok(v) => v,
            Err(_) => {
                let _ = outbound_tx.send(serde_json::json!({
                    "type": "error",
                    "message": "Invalid JSON"
                }));
                continue;
            }
        };

        let msg_type = parsed["type"].as_str().unwrap_or("");
        if msg_type != "message" {
            continue;
        }

        let content = parsed["content"].as_str().unwrap_or("").to_string();
        if content.is_empty() {
            continue;
        }
        println!("  💬 [web_dashboard] from browser: {}", content);
        println!("  ⏳ Processing message...");
        let started_at = Instant::now();

        // Process message with the autonomous task-engine path (same as iMessage flow).
        let provider_label = state
            .config
            .lock()
            .default_provider
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        let _ = state.event_tx.send(serde_json::json!({
            "type": "agent_start",
            "provider": provider_label,
            "model": state.model,
        }));

        history.push(ChatMessage::user(&content));

        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<String>(64);
        let outbound_for_delta = outbound_tx.clone();
        let delta_forwarder = tokio::spawn(async move {
            while let Some(delta) = delta_rx.recv().await {
                if delta == DRAFT_CLEAR_SENTINEL {
                    continue;
                }
                let _ = outbound_for_delta.send(serde_json::json!({
                    "type": "chunk",
                    "content": delta
                }));
            }
        });

        let progress_outbound = outbound_tx.clone();
        let progress_reporter: TaskProgressReporter = Arc::new(move |progress: String| {
            println!("  🤖 [progress][web_dashboard]: {}", progress);
            let _ = progress_outbound.send(serde_json::json!({
                "type": "message",
                "content": progress
            }));
        });

        let autonomous_result = if let Some(engine) = state.task_engine.as_ref() {
            let req = TaskRunRequest {
                channel: "web",
                sender_key: sender_key.as_str(),
                reply_target: sender_key.as_str(),
                original_request: content.as_str(),
                provider: state.provider.as_ref(),
                history: &mut history,
                tools_registry: state.runtime_tools.as_ref(),
                observer: state.observer.as_ref(),
                provider_name: provider_label.as_str(),
                model: state.model.as_str(),
                temperature: state.temperature,
                multimodal: &state.multimodal,
                max_tool_iterations: state.max_tool_iterations,
                cancellation_token: None,
                on_delta: Some(delta_tx.clone()),
                hooks: state.hooks.as_deref(),
                excluded_tools: state.non_cli_excluded_tools.as_ref(),
                progress_reporter: Some(progress_reporter),
            };
            TaskEngine::run_task(req, engine.as_ref())
                .await
                .map(|outcome| outcome.final_response)
        } else {
            run_tool_call_loop(
                state.provider.as_ref(),
                &mut history,
                state.runtime_tools.as_ref(),
                state.observer.as_ref(),
                provider_label.as_str(),
                state.model.as_str(),
                state.temperature,
                true,
                None,
                "web",
                &state.multimodal,
                state.max_tool_iterations,
                None,
                Some(delta_tx.clone()),
                state.hooks.as_deref(),
                state.non_cli_excluded_tools.as_ref(),
            )
            .await
        };

        drop(delta_tx);
        let _ = delta_forwarder.await;

        match autonomous_result {
            Ok(response) => {
                println!(
                    "  🤖 Reply ({}ms): {}",
                    started_at.elapsed().as_millis(),
                    response
                );
                let _ = outbound_tx.send(serde_json::json!({
                    "type": "done",
                    "full_response": response,
                }));
                let _ = state.event_tx.send(serde_json::json!({
                    "type": "agent_end",
                    "provider": provider_label,
                    "model": state.model,
                }));
            }
            Err(e) => {
                let sanitized = crate::providers::sanitize_api_error(&e.to_string());
                eprintln!(
                    "  ❌ LLM error after {}ms: {}",
                    started_at.elapsed().as_millis(),
                    sanitized
                );
                let _ = outbound_tx.send(serde_json::json!({
                    "type": "error",
                    "message": sanitized,
                }));
                let _ = state.event_tx.send(serde_json::json!({
                    "type": "error",
                    "component": "ws_chat",
                    "message": sanitized,
                }));
            }
        }
    }

    drop(outbound_tx);
    let _ = writer.await;
}

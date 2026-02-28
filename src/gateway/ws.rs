//! WebSocket agent chat handler.
//!
//! Protocol:
//! ```text
//! Client -> Server: {"type":"message","content":"Hello"}
//! Server -> Client: {"type":"progress","content":"Round 1 running..."}
//! Server -> Client: {"type":"chunk","content":"Hi! "}
//! Server -> Client: {"type":"tool_call","name":"shell","args":{...}}
//! Server -> Client: {"type":"tool_result","name":"shell","output":"..."}
//! Server -> Client: {"type":"done","full_response":"..."}
//! ```

use super::AppState;
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

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (sender, mut receiver) = socket.split();
    let sender = Arc::new(tokio::sync::Mutex::new(sender));

    while let Some(msg) = receiver.next().await {
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
                let err = serde_json::json!({"type": "error", "message": "Invalid JSON"});
                let mut ws_sender = sender.lock().await;
                let _ = ws_sender.send(Message::Text(err.to_string().into())).await;
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

        // Process message with the LLM provider
        let provider_label = state
            .config
            .lock()
            .default_provider
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        // Broadcast agent_start event
        let _ = state.event_tx.send(serde_json::json!({
            "type": "agent_start",
            "provider": provider_label,
            "model": state.model,
        }));

        // Use the same autonomous task engine path as iMessage for parity.
        // Progress frames are sent through a single channel worker so ordering is stable.
        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<String>();
        let ws_sender_for_progress = Arc::clone(&sender);
        let progress_pump = tokio::spawn(async move {
            while let Some(progress) = progress_rx.recv().await {
                let progress_frame = serde_json::json!({
                    "type": "progress",
                    "content": progress,
                });
                let mut ws_sender = ws_sender_for_progress.lock().await;
                if ws_sender
                    .send(Message::Text(progress_frame.to_string().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let progress_tx_for_reporter = progress_tx.clone();
        let progress_reporter: crate::agent::task_engine::TaskProgressReporter =
            Arc::new(move |progress: String| {
                let _ = progress_tx_for_reporter.send(progress);
            });

        let config = state.config.lock().clone();
        let started_at = Instant::now();
        let task_result = crate::agent::loop_::process_message_with_channel_with_progress(
            config,
            &content,
            "web_dashboard",
            Some(progress_reporter),
        )
        .await;

        // Ensure all queued progress frames are flushed before terminal done/error.
        drop(progress_tx);
        let _ = progress_pump.await;

        match task_result {
            Ok(response) => {
                println!(
                    "  🤖 Reply ({}ms): {}",
                    started_at.elapsed().as_millis(),
                    response
                );
                // Send the full response as a done message
                let done = serde_json::json!({
                    "type": "done",
                    "full_response": response,
                });
                let mut ws_sender = sender.lock().await;
                let _ = ws_sender.send(Message::Text(done.to_string().into())).await;

                // Broadcast agent_end event
                let _ = state.event_tx.send(serde_json::json!({
                    "type": "agent_end",
                    "provider": provider_label,
                    "model": state.model,
                }));
            }
            Err(e) => {
                let sanitized = crate::providers::sanitize_api_error(&e.to_string());
                println!(
                    "  🤖 Reply ({}ms): [Error] {}",
                    started_at.elapsed().as_millis(),
                    sanitized
                );
                let err = serde_json::json!({
                    "type": "error",
                    "message": sanitized,
                });
                let mut ws_sender = sender.lock().await;
                let _ = ws_sender.send(Message::Text(err.to_string().into())).await;

                // Broadcast error event
                let _ = state.event_tx.send(serde_json::json!({
                    "type": "error",
                    "component": "ws_chat",
                    "message": sanitized,
                }));
            }
        }
    }
}

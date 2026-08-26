//! Tauri commands over the existing [`ConversationService`]
//! (Phase 10.2 вЂ” Tauri Command Layer).
//!
//! Each command is a thin translation of Tauri inputs/outputs: it delegates to
//! the existing application-layer conversation service and converts its
//! classified errors into safe [`CommandError`] values. No new business logic
//! or repository access lives here.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
#![allow(clippy::needless_pass_by_value)]

use tauri::{AppHandle, Manager, State};

use crate::application::conversations::ConversationService;
use crate::application::execution::AiResponse;
use crate::infrastructure::database::Database;
use crate::infrastructure::repository::conversations::Conversation;
use crate::infrastructure::repository::messages::Message;

use super::error::{CommandError, ErrorKind};

/// Create a new conversation and return its schema-assigned `id`.
#[tauri::command]
pub(crate) fn create_conversation(
    title: String,
    db: State<'_, Database>,
) -> Result<i64, CommandError> {
    ConversationService::new(db.inner())
        .create(&title)
        .map_err(Into::into)
}

/// List all conversations (the conversation history / sidebar list).
#[tauri::command]
pub(crate) fn list_conversations(
    db: State<'_, Database>,
) -> Result<Vec<Conversation>, CommandError> {
    ConversationService::new(db.inner())
        .list()
        .map_err(Into::into)
}

/// Read the message history for one conversation.
#[tauri::command]
pub(crate) fn conversation_history(
    conversation_id: i64,
    db: State<'_, Database>,
) -> Result<Vec<Message>, CommandError> {
    ConversationService::new(db.inner())
        .history(conversation_id)
        .map_err(Into::into)
}

/// Rename a conversation.
#[tauri::command]
pub(crate) fn rename_conversation(
    id: i64,
    title: String,
    db: State<'_, Database>,
) -> Result<(), CommandError> {
    ConversationService::new(db.inner())
        .rename(id, &title)
        .map_err(Into::into)
}

/// Archive a conversation.
#[tauri::command]
pub(crate) fn archive_conversation(id: i64, db: State<'_, Database>) -> Result<(), CommandError> {
    ConversationService::new(db.inner())
        .archive(id)
        .map_err(Into::into)
}

/// Restore an archived conversation to the active state.
#[tauri::command]
pub(crate) fn restore_conversation(id: i64, db: State<'_, Database>) -> Result<(), CommandError> {
    ConversationService::new(db.inner())
        .restore(id)
        .map_err(Into::into)
}

/// Delete a conversation and the messages/attachments that cascade from it.
#[tauri::command]
pub(crate) fn delete_conversation(id: i64, db: State<'_, Database>) -> Result<(), CommandError> {
    ConversationService::new(db.inner())
        .delete(id)
        .map_err(Into::into)
}

/// Send a user message to a conversation and return the normalized AI
/// response, which is also persisted as the assistant message. Any draft
/// attachment ids supplied are linked to the created user message before the
/// request is executed (FR-008).
///
/// # Threading (BUG-005 and its regression)
///
/// The send pipeline is deliberately synchronous end to end (`SQLite` through
/// `Mutex<Connection>`, `fs::read` + base64 attachment encoding, and the
/// provider HTTP round trip performed with `reqwest::blocking`). It must run
/// on a **plain thread**:
///
/// - On the main thread it froze the entire UI for the request duration, so
///   the command is declared `async`.
/// - An `async` Tauri command runs on an async-runtime **worker**, which is
///   equally wrong: in debug builds `reqwest::blocking` builds and drops a
///   throwaway tokio shell runtime on the calling thread (`wait.rs::enter`),
///   which panics inside an active runtime вЂ” killing the command task and
///   leaving the frontend invoke promise unresolved forever (the reported
///   "sends hang indefinitely" regression).
///
/// The body therefore moves the whole pipeline onto the runtime's dedicated
/// **blocking pool** via [`tauri::async_runtime::spawn_blocking`]: plain OS
/// threads with no ambient async context, where the blocking stack behaves
/// exactly as on any other thread. The UI stays responsive, no async worker
/// is occupied, and the frontend contract is unchanged вЂ” `invoke` still
/// resolves with the response or rejects with a classified error.
#[tauri::command]
pub(crate) async fn send_message(
    app: AppHandle,
    conversation_id: i64,
    content: String,
    provider: String,
    model: String,
    attachment_ids: Vec<i64>,
) -> Result<AiResponse, CommandError> {
    // Owned handle so the managed state can be reached from the blocking
    // thread (borrowed `State<'_, _>` cannot cross into `'static` work).
    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let db = handle.state::<Database>();
        ConversationService::new(db.inner())
            .send_message(
                conversation_id,
                &content,
                &provider,
                &model,
                &attachment_ids,
            )
            .map_err(Into::into)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => {
            // Only reachable if the blocking task panicked: report a safe,
            // classified failure instead of leaving the promise dangling.
            log::error!("send_message blocking task failed: {err}");
            Err(CommandError::new(
                ErrorKind::Request,
                "the message could not be processed",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::application::execution::{AiMessage, AiRequest, AiRole, ProviderExecutor};
    use crate::infrastructure::providers::openai::{OpenAiExecutor, PROVIDER_NAME};

    /// Regression for the BUG-005 follow-up ("all sends hang indefinitely"):
    /// the send pipeline is synchronous end to end (`SQLite`, attachment file
    /// reads + base64, and a `reqwest::blocking` HTTP round trip) and must
    /// run on the async runtime's dedicated blocking pool вЂ” never on the
    /// main thread (UI freeze) and never directly inside an async worker
    /// (debug-build `reqwest::blocking` panics there while building its
    /// shell runtime, leaving the frontend invoke promise unresolved).
    ///
    /// This drives the exact production scheduling shape of the
    /// `send_message` command вЂ” `spawn_blocking` awaited from the async
    /// runtime вЂ” through a real `reqwest::blocking` round trip against a
    /// local server, proving the blocking stack completes when entered this
    /// way and returns its response to the awaiting command.
    #[test]
    fn send_pipeline_completes_when_run_on_the_runtime_blocking_pool() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let addr = listener.local_addr().expect("local address");

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut raw = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        raw.extend_from_slice(&buf[..n]);
                        if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            // A minimal valid OpenAI-style success body.
            let body = r#"{"model":"gpt-5.6-terra","choices":[{"message":{"content":"pong"}}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
            stream.flush().expect("flush response");
        });

        let executor = OpenAiExecutor::with_endpoint(format!("http://{addr}"));
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "gpt-5.6-terra".to_string(),
            messages: vec![AiMessage {
                role: AiRole::User,
                content: "Hello".to_string(),
                attachments: Vec::new(),
            }],
            tools: Vec::new(),
        };

        // The production scheduling of `send_message`: an async command
        // awaits the whole blocking pipeline moved onto the blocking pool.
        let ai = tauri::async_runtime::block_on(async {
            tauri::async_runtime::spawn_blocking(move || {
                executor.execute(&request, "sk-secret-example")
            })
            .await
            .expect("blocking task joins without panicking")
            .expect("provider round trip succeeds")
        });
        server.join().expect("server thread joins");

        assert_eq!(ai.content, "pong");
        assert_eq!(ai.model, "gpt-5.6-terra");
    }

    /// Negative control documenting WHY the pipeline must go through
    /// `spawn_blocking`: driven directly inside an async-runtime worker (the
    /// regression's broken scheduling), the debug-build `reqwest::blocking`
    /// stack aborts the task instead of completing it вЂ” which is precisely
    /// what left the frontend invoke promise unresolved forever. Debug-only
    /// because reqwest's shell-runtime check is `cfg(debug_assertions)`.
    /// If this ever stops failing after a dependency upgrade, re-read the
    /// `send_message` threading docs before touching anything.
    #[test]
    #[cfg(debug_assertions)]
    fn negative_control_provider_call_directly_inside_an_async_worker_aborts() {
        // Unreachable endpoint: if the call somehow ran, it fails fast with
        // a connect error instead of hanging; the assertion below expects
        // neither outcome but a dead task.
        let executor = OpenAiExecutor::with_endpoint("http://127.0.0.1:1".to_string());
        let request = AiRequest {
            provider: PROVIDER_NAME.to_string(),
            model: "gpt-5.6-terra".to_string(),
            messages: vec![AiMessage {
                role: AiRole::User,
                content: "Hello".to_string(),
                attachments: Vec::new(),
            }],
            tools: Vec::new(),
        };

        let joined = tauri::async_runtime::block_on(async {
            tauri::async_runtime::spawn(
                async move { executor.execute(&request, "sk-secret-example") },
            )
            .await
        });

        assert!(
            joined.is_err(),
            "reqwest::blocking unexpectedly completed inside an async worker; \
             the send_message threading constraint may have been lifted upstream"
        );
    }
}

//! Durable Agent conversation commands: search, open, append, archive and delete.

use serde::Serialize;
use tauri::State;

use crate::state::AppState;
use yukinal_database::models::{ChatMessage, ChatMessageRole, ChatSession};

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 100;
const MAX_QUERY_CHARS: usize = 200;
const MAX_ID_CHARS: usize = 256;
const MAX_TITLE_CHARS: usize = 200;
const MAX_CONTENT_CHARS: usize = 100_000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSessionListResponse {
    pub sessions: Vec<ChatSession>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSessionDetailResponse {
    pub session: ChatSession,
    pub messages: Vec<ChatMessage>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSessionResponse {
    pub session: ChatSession,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessageResponse {
    pub message: ChatMessage,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatDeleteResponse {
    pub deleted: bool,
}

#[tauri::command]
pub fn chat_session_list(
    state: State<'_, AppState>,
    query: Option<String>,
    archived: Option<bool>,
    limit: Option<usize>,
) -> Result<ChatSessionListResponse, String> {
    let query = normalized_optional(query, MAX_QUERY_CHARS, "query")?;
    let limit = bounded_limit(limit)?;
    let sessions = state
        .database
        .chat()
        .list(query.as_deref(), archived, limit)
        .map_err(|error| error.to_string())?;
    Ok(ChatSessionListResponse { sessions })
}

#[tauri::command]
pub fn chat_session_get(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<ChatSessionDetailResponse, String> {
    let session_id = validate_id(&session_id, "session id")?;
    let session = state
        .database
        .chat()
        .get(&session_id)
        .map_err(|error| error.to_string())?;
    let messages = state
        .database
        .chat()
        .messages(&session_id)
        .map_err(|error| error.to_string())?;
    Ok(ChatSessionDetailResponse { session, messages })
}

#[tauri::command]
pub fn chat_session_create(
    state: State<'_, AppState>,
    session_id: Option<String>,
    workspace_id: Option<String>,
    server_id: Option<String>,
    title: String,
) -> Result<ChatSessionResponse, String> {
    let id = session_id
        .map(|value| validate_id(&value, "session id"))
        .transpose()?
        .unwrap_or_else(|| crate::commands::server::next_id("ses"));
    let workspace_id = normalized_optional(workspace_id, MAX_ID_CHARS, "workspace id")?;
    let server_id = normalized_optional(server_id, MAX_ID_CHARS, "server id")?;
    if let Some(server_id) = server_id.as_deref() {
        if !is_stable_server_id(server_id) {
            return Err("server id must be an opaque srv_ id".to_string());
        }
    }
    let title = validate_text(&title, MAX_TITLE_CHARS, "title")?;
    let now = yukinal_core::sidecar::iso8601_now();
    let session = ChatSession {
        id,
        workspace_id,
        server_id,
        title,
        created_at: now.clone(),
        updated_at: now,
        archived_at: None,
        message_count: 0,
        last_message_preview: None,
    };
    state
        .database
        .chat()
        .create(&session)
        .map_err(|error| error.to_string())?;
    Ok(ChatSessionResponse { session })
}

#[tauri::command]
pub fn chat_message_append(
    state: State<'_, AppState>,
    session_id: String,
    message_id: Option<String>,
    role: String,
    content: String,
    trace_id: Option<String>,
    created_at: Option<String>,
) -> Result<ChatMessageResponse, String> {
    let session_id = validate_id(&session_id, "session id")?;
    let message_id = message_id
        .map(|value| validate_id(&value, "message id"))
        .transpose()?
        .unwrap_or_else(|| crate::commands::server::next_id("msg"));
    let role = ChatMessageRole::from_db(role.trim())
        .ok_or_else(|| "role must be user, assistant, tool or system".to_string())?;
    let content = validate_text(&content, MAX_CONTENT_CHARS, "content")?;
    let trace_id = normalized_optional(trace_id, MAX_ID_CHARS, "trace id")?;
    let created_at = created_at
        .map(|value| validate_text(&value, 80, "created at"))
        .transpose()?
        .unwrap_or_else(yukinal_core::sidecar::iso8601_now);
    let message = ChatMessage {
        id: message_id,
        session_id,
        role,
        content,
        trace_id,
        created_at,
    };
    state
        .database
        .chat()
        .append_message(&message)
        .map_err(|error| error.to_string())?;
    Ok(ChatMessageResponse { message })
}

#[tauri::command]
pub fn chat_session_archive(
    state: State<'_, AppState>,
    session_id: String,
    archived: bool,
) -> Result<ChatSessionResponse, String> {
    let session_id = validate_id(&session_id, "session id")?;
    let archived_at = archived.then(yukinal_core::sidecar::iso8601_now);
    let session = state
        .database
        .chat()
        .set_archived(&session_id, archived_at.as_deref())
        .map_err(|error| error.to_string())?;
    Ok(ChatSessionResponse { session })
}

#[tauri::command]
pub fn chat_session_delete(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<ChatDeleteResponse, String> {
    let session_id = validate_id(&session_id, "session id")?;
    state
        .database
        .chat()
        .delete(&session_id)
        .map_err(|error| error.to_string())?;
    Ok(ChatDeleteResponse { deleted: true })
}

fn bounded_limit(limit: Option<usize>) -> Result<usize, String> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(format!(
            "chat session limit must be between 1 and {MAX_LIMIT}"
        ));
    }
    Ok(limit)
}

fn normalized_optional(
    value: Option<String>,
    max_chars: usize,
    field: &str,
) -> Result<Option<String>, String> {
    value
        .map(|value| validate_text(&value, max_chars, field))
        .transpose()
}

fn validate_id(value: &str, field: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > MAX_ID_CHARS {
        return Err(format!(
            "{field} must be between 1 and {MAX_ID_CHARS} characters"
        ));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(format!("{field} contains unsupported characters"));
    }
    Ok(value.to_string())
}

fn validate_text(value: &str, max_chars: usize, field: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max_chars {
        return Err(format!(
            "{field} must be between 1 and {max_chars} characters"
        ));
    }
    Ok(value.to_string())
}

fn is_stable_server_id(value: &str) -> bool {
    value.len() > 4
        && value.starts_with("srv_")
        && value
            .bytes()
            .skip(4)
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::{bounded_limit, is_stable_server_id, validate_id, validate_text};

    #[test]
    fn chat_inputs_are_bounded_and_opaque() {
        assert_eq!(validate_id("ses_01", "session id").unwrap(), "ses_01");
        assert!(validate_id("session id", "session id").is_err());
        assert!(validate_text("   ", 10, "title").is_err());
        assert!(bounded_limit(Some(101)).is_err());
        assert!(is_stable_server_id("srv_01abc"));
        assert!(!is_stable_server_id("srv_ABC"));
    }
}

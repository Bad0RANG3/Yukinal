//! Durable Agent conversation commands: search, open, append, archive and delete.

use serde::Serialize;
use tauri::State;

use crate::state::AppState;
use yukinal_core::ids::is_stable_server_id;
use yukinal_database::models::{ChatMessage, ChatMessageRole, ChatSession, ChatSessionCounts};

use super::agent_run::{validate_prompt_parts, PromptPart};

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 100;
/// Matches the `offset` bound in `packages/shared/src/schemas/ipc.ts`. Beyond this the
/// caller is not paging a local conversation list, it is sweeping the table.
const MAX_OFFSET: usize = 10_000;
const MAX_QUERY_CHARS: usize = 200;
const MAX_ID_CHARS: usize = 256;
const MAX_TITLE_CHARS: usize = 200;
const MAX_CONTENT_CHARS: usize = 100_000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSessionListResponse {
    pub sessions: Vec<ChatSession>,
    /// Counts for the same search, ignoring the archive filter, so the record view can
    /// label 进行中 / 已归档 / 全部 without fetching all three lists.
    pub counts: ChatSessionCounts,
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
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ChatSessionListResponse, String> {
    let query = normalized_optional(query, MAX_QUERY_CHARS, "query")?;
    let limit = bounded_limit(limit)?;
    let offset = bounded_offset(offset)?;
    let repository = state.database.chat();
    let sessions = repository
        .list(query.as_deref(), archived, limit, offset)
        .map_err(|error| error.to_string())?;
    let counts = repository
        .counts(query.as_deref())
        .map_err(|error| error.to_string())?;
    Ok(ChatSessionListResponse { sessions, counts })
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
#[allow(clippy::too_many_arguments)]
pub fn chat_message_append(
    state: State<'_, AppState>,
    session_id: String,
    message_id: Option<String>,
    role: String,
    content: String,
    parts: Option<Vec<PromptPart>>,
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
    let parts = parts.filter(|items| !items.is_empty());
    if let Some(parts) = parts.as_deref() {
        validate_prompt_parts(parts)?;
    }
    let content = if content.trim().is_empty() {
        if parts.is_none() {
            return Err("content must not be empty without prompt parts".into());
        }
        String::new()
    } else {
        validate_text(&content, MAX_CONTENT_CHARS, "content")?
    };
    let parts = parts
        .map(|parts| {
            parts
                .into_iter()
                .map(serde_json::to_value)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()
        .map_err(|error| format!("failed to encode prompt parts: {error}"))?;
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
        parts,
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
pub fn chat_session_rename(
    state: State<'_, AppState>,
    session_id: String,
    title: String,
) -> Result<ChatSessionResponse, String> {
    let session_id = validate_id(&session_id, "session id")?;
    let title = validate_text(&title, MAX_TITLE_CHARS, "title")?;
    let session = state
        .database
        .chat()
        .rename(&session_id, &title)
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

fn bounded_offset(offset: Option<usize>) -> Result<usize, String> {
    let offset = offset.unwrap_or(0);
    if offset > MAX_OFFSET {
        return Err(format!("chat session offset must be at most {MAX_OFFSET}"));
    }
    Ok(offset)
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

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::{
        bounded_limit, bounded_offset, is_stable_server_id, validate_id, validate_text,
        ChatDeleteResponse, ChatMessageResponse, ChatSessionDetailResponse,
        ChatSessionListResponse, ChatSessionResponse,
    };
    use yukinal_database::models::{ChatMessage, ChatMessageRole, ChatSession, ChatSessionCounts};

    /// 契约 fixture 是两侧共同解析的那份 JSON；这里断言的是 Rust 那一半 ——
    /// serde 的输出必须逐字节等于 TypeScript 那一半（`schemas/ipc.test.ts`）解析的文件。
    ///
    /// 对话记录这一族过去一份 Rust 断言都没有：Rust 侧改掉字段名时，仓库里不会有任何
    /// 检查变红（`docs/limitations.md` 的「当前限制」曾经就是这么写的）。记录视图现在依赖
    /// `chat_session_list` 的 `counts` 与 `chat_session_rename`，所以这一族整体补上。
    mod fixtures {
        pub const LIST: &str =
            include_str!("../../../../../packages/shared/fixtures/ipc/chat_session_list.json");
        pub const GET: &str =
            include_str!("../../../../../packages/shared/fixtures/ipc/chat_session_get.json");
        pub const CREATE: &str =
            include_str!("../../../../../packages/shared/fixtures/ipc/chat_session_create.json");
        pub const ARCHIVE: &str =
            include_str!("../../../../../packages/shared/fixtures/ipc/chat_session_archive.json");
        pub const RENAME: &str =
            include_str!("../../../../../packages/shared/fixtures/ipc/chat_session_rename.json");
        pub const DELETE: &str =
            include_str!("../../../../../packages/shared/fixtures/ipc/chat_session_delete.json");
        pub const MESSAGE_APPEND: &str =
            include_str!("../../../../../packages/shared/fixtures/ipc/chat_message_append.json");
    }

    fn fixture(raw: &str) -> Value {
        serde_json::from_str(raw).expect("contract fixture must be valid JSON")
    }

    /// fixture 里那段对话；每个用例只改自己要说的那一处。
    fn sample_session() -> ChatSession {
        ChatSession {
            id: "ses_01".into(),
            workspace_id: None,
            server_id: None,
            title: "排查 API 错误".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            archived_at: None,
            message_count: 0,
            last_message_preview: None,
        }
    }

    #[test]
    fn chat_inputs_are_bounded_and_opaque() {
        assert_eq!(validate_id("ses_01", "session id").unwrap(), "ses_01");
        assert!(validate_id("session id", "session id").is_err());
        assert!(validate_text("   ", 10, "title").is_err());
        assert!(bounded_limit(Some(101)).is_err());
        assert!(is_stable_server_id("srv_01abc"));
        assert!(!is_stable_server_id("srv_ABC"));
        // 聊天记录曾经与审计管道一起接受下划线；规则现在只有 `yukinal_core::ids` 一份。
        assert!(!is_stable_server_id("srv_01_abc"));
    }

    #[test]
    fn chat_paging_offset_defaults_to_the_first_page_and_is_capped() {
        // Omitted means "first page", not "reject": an older UI must keep working.
        assert_eq!(bounded_offset(None).unwrap(), 0);
        assert_eq!(bounded_offset(Some(0)).unwrap(), 0);
        assert_eq!(bounded_offset(Some(10_000)).unwrap(), 10_000);
        assert!(bounded_offset(Some(10_001)).is_err());
    }

    #[test]
    fn a_created_session_serializes_to_the_contract_fixture() {
        let actual = serde_json::to_value(ChatSessionResponse {
            session: sample_session(),
        })
        .expect("serialize create response");
        assert_eq!(actual, fixture(fixtures::CREATE));
    }

    #[test]
    fn a_session_detail_serializes_to_the_contract_fixture() {
        let actual = serde_json::to_value(ChatSessionDetailResponse {
            session: ChatSession {
                updated_at: "2026-01-01T00:01:00Z".into(),
                ..sample_session()
            },
            messages: Vec::new(),
        })
        .expect("serialize detail response");
        assert_eq!(actual, fixture(fixtures::GET));
        // `messages` is present even when the conversation has none: the UI replaces its
        // transcript with this array, and an omitted field would read as "not loaded".
        assert_eq!(
            actual
                .get("messages")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0)
        );
    }

    #[test]
    fn an_archived_session_serializes_to_the_contract_fixture() {
        let actual = serde_json::to_value(ChatSessionResponse {
            session: ChatSession {
                updated_at: "2026-01-01T00:01:00Z".into(),
                archived_at: Some("2026-01-01T00:02:00Z".into()),
                message_count: 1,
                last_message_preview: Some("检查 API 错误".into()),
                ..sample_session()
            },
        })
        .expect("serialize archive response");
        assert_eq!(actual, fixture(fixtures::ARCHIVE));
    }

    /// 重命名返回的是**整个会话行**，不是「已改名」这个事实：界面要用新的标题就地更新
    /// 那一行，还得保留 `updatedAt` 不变（改名不是活动，不该把记录顶到列表最上面）。
    #[test]
    fn a_renamed_session_serializes_to_the_contract_fixture() {
        let actual = serde_json::to_value(ChatSessionResponse {
            session: ChatSession {
                server_id: Some("srv_01abc".into()),
                title: "排查 API 错误（已定位）".into(),
                updated_at: "2026-01-01T00:01:00Z".into(),
                message_count: 1,
                last_message_preview: Some("upstream 连接被拒，容器在重启循环里。".into()),
                ..sample_session()
            },
        })
        .expect("serialize rename response");
        assert_eq!(actual, fixture(fixtures::RENAME));
    }

    /// 列表响应带着 `counts`：筛选标签写的是「这个关键字下有多少条」，而列表本身只有
    /// 一页。少了这个字段，界面就只能数手里这一页，那个数字看起来一样但是错的。
    #[test]
    fn a_session_list_serializes_to_the_contract_fixture_with_its_counts() {
        let actual = serde_json::to_value(ChatSessionListResponse {
            sessions: Vec::new(),
            counts: ChatSessionCounts {
                active: 0,
                archived: 0,
            },
        })
        .expect("serialize list response");
        assert_eq!(actual, fixture(fixtures::LIST));
        let counts = actual.get("counts").expect("counts must be on the wire");
        assert_eq!(counts, &json!({ "active": 0, "archived": 0 }));
    }

    #[test]
    fn a_deleted_session_serializes_to_the_contract_fixture() {
        let actual = serde_json::to_value(ChatDeleteResponse { deleted: true })
            .expect("serialize delete response");
        assert_eq!(actual, fixture(fixtures::DELETE));
    }

    #[test]
    fn an_appended_message_serializes_to_the_contract_fixture() {
        let actual = serde_json::to_value(ChatMessageResponse {
            message: ChatMessage {
                id: "msg_01".into(),
                session_id: "ses_01".into(),
                role: ChatMessageRole::User,
                content: "检查 API 错误".into(),
                parts: None,
                trace_id: None,
                created_at: "2026-01-01T00:01:00Z".into(),
            },
        })
        .expect("serialize append response");
        assert_eq!(actual, fixture(fixtures::MESSAGE_APPEND));
        // Optional fields stay off the wire rather than becoming null: the TS schema is a
        // strictObject with an optional `traceId`, and `null` would fail it.
        assert_eq!(actual["message"].get("traceId"), None);
    }
}

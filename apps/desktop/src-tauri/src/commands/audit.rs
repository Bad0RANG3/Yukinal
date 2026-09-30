//! Audit projection for Agent tool results.
//!
//! Secrets and file bodies are intentionally reduced before anything reaches SQLite or a
//! UI activity event. The public surface is the bounded projection, not the raw Agent frame.

use super::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentToolResultEvent {
    trace_id: String,
    step_id: String,
    call_id: String,
    tool_name: String,
    input: Value,
    target: AgentToolTarget,
    risk_level: RiskLevel,
    decision: PermissionMode,
    approved_by: Option<AgentApprovalSource>,
    status: ToolExecutionStatus,
    output_summary: String,
    execution_state: Option<String>,
    error: Option<String>,
    error_code: Option<String>,
    started_at: String,
    ended_at: String,
    duration_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentToolTarget {
    host: AgentToolHost,
    server_id: Option<String>,
    workspace_id: Option<String>,
    environment: Environment,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum AgentToolHost {
    Local,
    Remote,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum AgentApprovalSource {
    User,
    Policy,
    Agent,
}

const MAX_AUDIT_TEXT_CHARS: usize = 4_000;
const MAX_AUDIT_INPUT_TEXT_CHARS: usize = 2_000;
const FILE_CONTENT_AUDIT_OMITTED: &str = "[file content omitted from audit]";

pub(crate) fn persist_agent_tool_result(app: &AppHandle, params: &Value) {
    let event = match serde_json::from_value::<AgentToolResultEvent>(params.clone()) {
        Ok(event) => event,
        Err(error) => {
            eprintln!("[agent] ignored malformed tool result event: {error}");
            return;
        }
    };

    if event.trace_id.trim().is_empty()
        || event.step_id.trim().is_empty()
        || event.call_id.trim().is_empty()
        || event.tool_name.trim().is_empty()
        || event.started_at.trim().is_empty()
        || event.ended_at.trim().is_empty()
        || event.duration_ms > i64::MAX as u64
    {
        eprintln!("[agent] ignored tool result event with an invalid audit identity");
        return;
    }

    if !is_valid_agent_target(&event.target) {
        eprintln!("[agent] ignored tool result event with an invalid target");
        return;
    }

    // 一个**结果**事件必须是终态。`AgentToolResultEvent.status` 的类型是完整的六值
    // `ToolExecutionStatus`（它按线上契约反序列化，契约里两者共用同一套名字），所以
    // 一个声称 `status: "running"` 的结果事件在类型上是合法的 —— 但它自相矛盾：它同时
    // 带着 `ended_at`。落库就会留下一行「还在跑、但已经结束」的记录，而那正是
    // 「崩溃中断的调用」本该由**缺失**表达的东西，审计里从此分不清两者。
    if !is_terminal_result_status(&event.status) {
        eprintln!("[agent] ignored tool result event whose status is not terminal");
        return;
    }

    let summary =
        if event.tool_name == "filesystem.read" && event.status == ToolExecutionStatus::Success {
            FILE_CONTENT_AUDIT_OMITTED.to_string()
        } else {
            safe_audit_summary(&event.output_summary, MAX_AUDIT_TEXT_CHARS)
        };
    let error = event
        .error
        .as_deref()
        .map(|value| safe_audit_summary(value, MAX_AUDIT_TEXT_CHARS))
        .or_else(|| {
            event
                .error_code
                .as_deref()
                .map(|code| format!("tool error: {code}"))
        })
        .or_else(|| (event.decision == PermissionMode::Deny).then(|| summary.clone()));
    // Failed calls also carry a bounded, redacted output summary. This is where
    // server.exec keeps its exit/result state when the run ends before a final
    // task-level event can explain it.
    let mut output = json!({ "summary": summary.clone() });
    if let Some(execution_state) = event
        .execution_state
        .as_deref()
        .filter(|state| matches!(*state, "timed_out" | "cancelled" | "result_unknown"))
    {
        output["executionState"] = json!(execution_state);
    }
    let output = Some(output);
    let outcome = match event.status {
        ToolExecutionStatus::Success => ActivityOutcome::Success,
        ToolExecutionStatus::Cancelled => ActivityOutcome::Cancelled,
        _ if event.decision == PermissionMode::Deny => ActivityOutcome::Denied,
        _ => ActivityOutcome::Failure,
    };
    let activity_description = output
        .as_ref()
        .and_then(|value| value.get("summary"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| error.clone());
    let tool_name = bounded_audit_text(&event.tool_name, 160);
    let record = ToolExecutionRecord {
        trace_id: event.trace_id.clone(),
        step_id: bounded_audit_text(&event.step_id, 160),
        call_id: bounded_audit_text(&event.call_id, 160),
        tool_name: tool_name.clone(),
        server_id: event.target.server_id.clone(),
        environment: event.target.environment,
        risk_level: event.risk_level,
        decision: event.decision,
        approved_by: event.approved_by.map(|source| match source {
            AgentApprovalSource::User => "user".to_string(),
            AgentApprovalSource::Policy => "policy".to_string(),
            AgentApprovalSource::Agent => "agent".to_string(),
        }),
        status: event.status,
        input: sanitize_audit_input(event.input),
        output,
        error,
        started_at: bounded_audit_text(&event.started_at, 80),
        ended_at: Some(bounded_audit_text(&event.ended_at, 80)),
        duration_ms: Some(event.duration_ms),
    };

    let state = app.state::<AppState>();
    if let Err(error) = state.database.executions().insert(&record) {
        eprintln!("[agent] failed to persist tool execution: {error}");
        return;
    }

    let activity = Activity {
        id: crate::commands::server::next_id("act"),
        server_id: record.server_id.clone(),
        workspace_id: event.target.workspace_id,
        r#type: ActivityType::AgentAction,
        title: format!("Agent 执行 {tool_name}"),
        description: activity_description,
        source: ActivitySource::Agent,
        actor: "agent".to_string(),
        reason: Some("Agent 按已解析目标和权限决策执行工具".to_string()),
        outcome: Some(outcome),
        trace_id: Some(record.trace_id.clone()),
        created_at: record
            .ended_at
            .clone()
            .unwrap_or_else(|| record.started_at.clone()),
    };
    if let Err(error) = state.database.activities().insert(&activity) {
        eprintln!("[agent] failed to persist tool activity: {error}");
    } else if let Ok(payload) = serde_json::to_value(&activity) {
        let _ = app.emit(&tauri_event_name("activity.created"), payload);
    }
}

fn is_valid_agent_target(target: &AgentToolTarget) -> bool {
    match (&target.host, target.server_id.as_deref()) {
        (AgentToolHost::Remote, Some(server_id)) => is_stable_server_id(server_id),
        (AgentToolHost::Local, None) => true,
        _ => false,
    }
}

pub(crate) fn bounded_audit_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let bounded: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{bounded}\n…[truncated]")
    } else {
        bounded
    }
}

pub(crate) fn safe_audit_summary(value: &str, max_chars: usize) -> String {
    let lowered = value.to_ascii_lowercase();
    const SENSITIVE_MARKERS: &[&str] = &[
        "api_key",
        "apikey",
        "authorization",
        "password",
        "passwd",
        "private_key",
        "private key",
        "secret",
        "token",
    ];
    if SENSITIVE_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
    {
        return "[sensitive output omitted]".to_string();
    }
    bounded_audit_text(value, max_chars)
}

pub(crate) fn sanitize_audit_input(value: Value) -> Value {
    match value {
        Value::Object(mut object) => {
            for (key, value) in &mut object {
                if is_sensitive_key(key) {
                    *value = Value::String("[redacted]".to_string());
                } else {
                    let nested = std::mem::take(value);
                    *value = sanitize_audit_input(nested);
                }
            }
            Value::Object(object)
        }
        Value::Array(values) => {
            Value::Array(values.into_iter().map(sanitize_audit_input).collect())
        }
        Value::String(value) => {
            Value::String(bounded_audit_text(&value, MAX_AUDIT_INPUT_TEXT_CHARS))
        }
        other => other,
    }
}

/// 一个工具**结果**的状态只能是终态。
///
/// `pending` / `running` / `waiting_approval` 描述的是「还没结束」，而结果事件同时带着
/// `ended_at`。放它们进来，审计里就会出现「已结束但还在跑」的行，而崩溃中断的调用在账本
/// 里是**没有行**——两者一旦混同，就再也分不出「跑完了」和「进程死了」。
///
/// 这条规则由契约保证（`TOOL_RESULT_STATUSES` 只有三个取值），但 Rust 侧的类型是从线上
/// 反序列化的完整六值枚举，所以必须在这里挡一次。
pub(crate) fn is_terminal_result_status(status: &ToolExecutionStatus) -> bool {
    matches!(
        status,
        ToolExecutionStatus::Success | ToolExecutionStatus::Failed | ToolExecutionStatus::Cancelled
    )
}

fn is_sensitive_key(value: &str) -> bool {
    let normalized: String = value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_lowercase())
        .collect();
    matches!(
        normalized.as_str(),
        "apikey"
            | "authorization"
            | "credential"
            | "credentials"
            | "password"
            | "passwd"
            | "privatekey"
            | "secret"
            | "token"
            | "content"
            // `filesystem.edit` 的参数里装着**任意文件内容**，和 `filesystem.write` 的
            // `content` 是同一种东西 —— 一份 `.env` 的改动片段里就有密钥。三者都要标成
            // 敏感，否则审计里只有 write 被抹掉，而 edit 把同一份内容原样留下。
            | "oldstring"
            | "newstring"
    )
}

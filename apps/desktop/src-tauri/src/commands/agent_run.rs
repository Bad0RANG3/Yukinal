//! Agent run commands: the UI sends a prompt; Rust resolves the provider +
//! credential (SQLite row + OS keychain), forwards `agent.run.start` to the
//! sidecar, and streams every observable step back as Tauri events.
//!
//! The sidecar never sees a key until this call: material rides only on the
//! transient JSON-RPC params (ADR 0001/0006; resolve secrets at the point of use).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::State;

use crate::commands::provider::resolve_api_key;
use crate::state::AppState;
use yukinal_core::provider::runtime_provider_config;
use yukinal_database::models::AiProviderConfig;

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PromptPart {
    Text {
        text: String,
    },
    Image {
        #[serde(rename = "mediaType")]
        media_type: String,
        data: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    File {
        #[serde(rename = "mediaType")]
        media_type: String,
        data: String,
        name: String,
    },
    Document {
        #[serde(rename = "mediaType")]
        media_type: String,
        data: String,
        name: String,
    },
    /// 一段有界的内联音频。与图片、PDF 共用同一个总预算：真正受约束的是那一帧。
    Audio {
        #[serde(rename = "mediaType")]
        media_type: String,
        data: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

const MAX_PROMPT_PARTS: usize = 10;
const MAX_PROMPT_IMAGES: usize = 4;
const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOTAL_INLINE_BYTES: usize = 5 * 1024 * 1024;
const MAX_PROMPT_TEXT_CHARS: usize = 100_000;
const MAX_IMAGE_NAME_CHARS: usize = 128;
const MAX_PROMPT_FILES: usize = 4;
const MAX_FILE_BYTES: usize = 256 * 1024;
const MAX_TOTAL_FILE_BYTES: usize = 512 * 1024;
const MAX_FILE_NAME_CHARS: usize = 128;
const MAX_PROMPT_DOCUMENTS: usize = 2;
const MAX_DOCUMENT_BYTES: usize = 3 * 1024 * 1024;
const MAX_DOCUMENT_NAME_CHARS: usize = 128;
const MAX_PROMPT_AUDIOS: usize = 2;
const MAX_AUDIO_BYTES: usize = 4 * 1024 * 1024;
const MAX_AUDIO_NAME_CHARS: usize = 128;

pub(crate) fn validate_prompt_parts(parts: &[PromptPart]) -> Result<(), String> {
    if parts.is_empty() || parts.len() > MAX_PROMPT_PARTS {
        return Err(format!(
            "prompt parts must contain between 1 and {MAX_PROMPT_PARTS} entries"
        ));
    }
    let mut image_count = 0usize;
    let mut inline_bytes = 0usize;
    let mut document_count = 0usize;
    let mut audio_count = 0usize;
    let mut file_count = 0usize;
    let mut file_bytes = 0usize;
    let mut text_chars = 0usize;
    for part in parts {
        match part {
            PromptPart::Text { text } => {
                text_chars = text_chars.saturating_add(text.chars().count());
                if text_chars > MAX_PROMPT_TEXT_CHARS {
                    return Err(format!(
                        "prompt text must be at most {MAX_PROMPT_TEXT_CHARS} characters"
                    ));
                }
            }
            PromptPart::Image {
                media_type,
                data,
                name,
            } => {
                image_count += 1;
                if image_count > MAX_PROMPT_IMAGES {
                    return Err(format!(
                        "a message may contain at most {MAX_PROMPT_IMAGES} images"
                    ));
                }
                if !matches!(
                    media_type.as_str(),
                    "image/png" | "image/jpeg" | "image/webp" | "image/gif"
                ) {
                    return Err(
                        "image mediaType must be image/png, image/jpeg, image/webp or image/gif"
                            .into(),
                    );
                }
                if let Some(name) = name {
                    let name = name.trim();
                    if name.is_empty()
                        || name.chars().count() > MAX_IMAGE_NAME_CHARS
                        || name.chars().any(char::is_control)
                    {
                        return Err(format!(
                            "image name must be between 1 and {MAX_IMAGE_NAME_CHARS} visible characters"
                        ));
                    }
                }
                let decoded = decoded_base64_bytes(data)?;
                if decoded > MAX_IMAGE_BYTES {
                    return Err(format!(
                        "each image must be at most {MAX_IMAGE_BYTES} decoded bytes"
                    ));
                }
                inline_bytes = inline_bytes.saturating_add(decoded);
                if inline_bytes > MAX_TOTAL_INLINE_BYTES {
                    return Err(format!(
                        "images, PDF documents and audio clips may total at most {MAX_TOTAL_INLINE_BYTES} decoded bytes"
                    ));
                }
            }
            PromptPart::Audio {
                media_type,
                data,
                name,
            } => {
                audio_count += 1;
                if audio_count > MAX_PROMPT_AUDIOS {
                    return Err(format!(
                        "a message may contain at most {MAX_PROMPT_AUDIOS} audio clips"
                    ));
                }
                if !matches!(
                    media_type.as_str(),
                    "audio/wav" | "audio/mpeg" | "audio/ogg" | "audio/flac"
                ) {
                    return Err(
                        "audio mediaType must be audio/wav, audio/mpeg, audio/ogg or audio/flac"
                            .into(),
                    );
                }
                let decoded = decoded_base64_bytes(data)?;
                if decoded > MAX_AUDIO_BYTES {
                    return Err(format!(
                        "each audio clip must be at most {MAX_AUDIO_BYTES} decoded bytes"
                    ));
                }
                inline_bytes = inline_bytes.saturating_add(decoded);
                if inline_bytes > MAX_TOTAL_INLINE_BYTES {
                    return Err(format!(
                        "images, PDF documents and audio clips may total at most {MAX_TOTAL_INLINE_BYTES} decoded bytes"
                    ));
                }
                if let Some(name) = name {
                    let name = name.trim();
                    if name.is_empty()
                        || name.chars().count() > MAX_AUDIO_NAME_CHARS
                        || name.chars().any(|character| {
                            character.is_control() || matches!(character, '/' | '\\')
                        })
                    {
                        return Err(format!(
                            "audio name must be between 1 and {MAX_AUDIO_NAME_CHARS} visible characters"
                        ));
                    }
                }
            }
            PromptPart::File {
                media_type,
                data,
                name,
            } => {
                file_count += 1;
                if file_count > MAX_PROMPT_FILES {
                    return Err(format!(
                        "a message may contain at most {MAX_PROMPT_FILES} text files"
                    ));
                }
                if media_type != "text/plain" {
                    return Err("text file mediaType must be text/plain".into());
                }
                if data.is_empty()
                    || data.chars().any(|character| {
                        character == '\0'
                            || (character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
                    })
                {
                    return Err("text file must be non-empty UTF-8 text".into());
                }
                if data.len() > MAX_FILE_BYTES {
                    return Err(format!(
                        "each text file must be at most {MAX_FILE_BYTES} UTF-8 bytes"
                    ));
                }
                file_bytes = file_bytes.saturating_add(data.len());
                if file_bytes > MAX_TOTAL_FILE_BYTES {
                    return Err(format!(
                        "text files may total at most {MAX_TOTAL_FILE_BYTES} UTF-8 bytes"
                    ));
                }
                let name = name.trim();
                if name.is_empty()
                    || name.chars().count() > MAX_FILE_NAME_CHARS
                    || name
                        .chars()
                        .any(|character| character.is_control() || matches!(character, '/' | '\\'))
                {
                    return Err(format!(
                        "text file name must be between 1 and {MAX_FILE_NAME_CHARS} visible characters"
                    ));
                }
            }
            PromptPart::Document {
                media_type,
                data,
                name,
            } => {
                document_count += 1;
                if document_count > MAX_PROMPT_DOCUMENTS {
                    return Err(format!(
                        "a message may contain at most {MAX_PROMPT_DOCUMENTS} PDF documents"
                    ));
                }
                if media_type != "application/pdf" {
                    return Err("document mediaType must be application/pdf".into());
                }
                let decoded = decoded_base64_bytes(data)?;
                if decoded > MAX_DOCUMENT_BYTES {
                    return Err(format!(
                        "each PDF document must be at most {MAX_DOCUMENT_BYTES} decoded bytes"
                    ));
                }
                inline_bytes = inline_bytes.saturating_add(decoded);
                if inline_bytes > MAX_TOTAL_INLINE_BYTES {
                    return Err(format!(
                        "images and PDF documents may total at most {MAX_TOTAL_INLINE_BYTES} decoded bytes"
                    ));
                }
                let name = name.trim();
                if name.is_empty()
                    || name.chars().count() > MAX_DOCUMENT_NAME_CHARS
                    || name
                        .chars()
                        .any(|character| character.is_control() || matches!(character, '/' | '\\'))
                {
                    return Err(format!(
                        "PDF name must be between 1 and {MAX_DOCUMENT_NAME_CHARS} visible characters"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn decoded_base64_bytes(value: &str) -> Result<usize, String> {
    if value.is_empty() || !value.len().is_multiple_of(4) {
        return Err("inline data must be base64 with a length divisible by four".into());
    }
    let bytes = value.as_bytes();
    let padding = if bytes.ends_with(b"==") {
        2
    } else if bytes.ends_with(b"=") {
        1
    } else {
        0
    };
    let content_len = bytes.len() - padding;
    if bytes[..content_len]
        .iter()
        .any(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'+' | b'/'))
        || bytes[content_len..].iter().any(|byte| *byte != b'=')
    {
        return Err("inline data must be canonical base64 without a data-URL prefix".into());
    }
    Ok(value.len() / 4 * 3 - padding)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStartResponse {
    pub run_id: String,
    /// Whether *this* call began execution, taken from the sidecar's answer rather than
    /// assumed. `false` means the message was admitted without being executed
    /// (`resume: false`) or that a retry hit a run that already exists — in both cases
    /// no `agent.*` event for this call may be expected.
    pub started: bool,
    /// The message was already admitted, so this call opened no second run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate: Option<bool>,
    /// This call executed a run that an earlier `resume: false` admitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resumed: Option<bool>,
    /// The run's outcome, present exactly when the request asked for `delivery: "sync"`.
    ///
    /// Forwarded as opaque JSON on purpose: this layer does not interpret a run result —
    /// it does not for the streaming case either, where the outcome arrives as events —
    /// and the shape is owned by `AgentRunResultSchema` in `@yukinal/shared`, which the UI
    /// validates. Skipping the field when absent keeps the async response byte-identical
    /// to the fixture that predates sync delivery.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStopResponse {
    pub stopped: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRespondResponse {
    pub accepted: bool,
}

/// Admission — the sidecar is told about the message and answers immediately.
const RUN_START_ASYNC_TIMEOUT: Duration = Duration::from_secs(10);

/// A sync run. The wait is bounded by the *loop's* own wall-clock bound (`maxRunMs`,
/// 15 minutes by default in `apps/agent/src/config.ts`, and the loop's timer is what
/// ends an overrunning run and produces its result). This timeout therefore has to sit
/// strictly outside that bound: a shorter one would abandon a run that is still allowed
/// to finish and lose the very result the caller asked for.
///
/// The consequence to know about: an operator who raises `YUKINAL_MAX_RUN_MS` past 16
/// minutes makes this the binding deadline instead, and a `sync` call then reports a
/// timeout rather than the run's own outcome. Raising one without the other is the only
/// way these two can disagree, and the failure is loud rather than silent.
const RUN_START_SYNC_TIMEOUT: Duration = Duration::from_secs(16 * 60);

/// How long to wait for `agent.run.start` to answer, which depends on `delivery`:
/// an async call answers as soon as the message is admitted (the run's outcome arrives
/// as events), while a sync call answers only once the run is over.
fn run_start_timeout(delivery: Option<&str>) -> Duration {
    match delivery {
        Some("sync") => RUN_START_SYNC_TIMEOUT,
        _ => RUN_START_ASYNC_TIMEOUT,
    }
}

/// 第一个启用的 AI provider；没有就明确报错（UI 引导去配置，不做假 provider）。
fn resolve_provider(
    state: &AppState,
    provider_id: Option<&str>,
) -> Result<AiProviderConfig, String> {
    let providers = state
        .database
        .providers()
        .list_ai()
        .map_err(|error| error.to_string())?;
    providers
        .into_iter()
        .find(|provider| {
            provider_id
                .map(|id| provider.id == id)
                .unwrap_or(provider.enabled)
                && provider.enabled
        })
        .ok_or_else(|| {
            "没有启用的 AI provider：请先到「设置 ▸ Provider」配置（baseUrl/model/API key）".into()
        })
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn agent_run_start(
    state: State<'_, AppState>,
    run_id: Option<String>,
    session_id: String,
    prompt: String,
    message_id: Option<String>,
    parts: Option<Vec<PromptPart>>,
    delivery: Option<String>,
    resume: Option<bool>,
    provider_id: Option<String>,
    model: Option<String>,
    workspace_id: Option<String>,
    focus_server_id: Option<String>,
    permission_mode: Option<String>,
    // Run mode (readonly / plan / goal). Forwarded verbatim: the sidecar's
    // permission engine owns the meaning, so Rust must not reinterpret it.
    mode: Option<String>,
    // Which permission policy must decide the run. Forwarded verbatim for the same
    // reason as `mode`: the sidecar's policy registry owns the ids, and it is what
    // refuses an unknown one. Rust never substitutes a policy of its own — a caller
    // that asked for a policy and silently got another one is the defect this
    // parameter exists to remove.
    policy_id: Option<String>,
) -> Result<RunStartResponse, String> {
    // Repair legacy databases before resolving the provider. The UI normally does
    // this through provider_list, but run.start must remain safe when invoked
    // directly or while the startup query is still refreshing.
    crate::commands::provider::normalize_active_provider(&state)?;
    let provider = resolve_provider(&state, provider_id.as_deref())?;
    let api_key = resolve_api_key(&state, &provider)?;
    let selected_model = model
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| provider.model.clone());
    if prompt.chars().count() > MAX_PROMPT_TEXT_CHARS {
        return Err(format!(
            "prompt must be at most {MAX_PROMPT_TEXT_CHARS} characters"
        ));
    }

    // Millisecond timestamps can collide when two submissions arrive in the
    // same tick; use the process-wide opaque id generator instead.
    let run_id = run_id
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| crate::commands::server::next_id("run"));
    let message_id = message_id.unwrap_or_else(|| format!("msg_{run_id}"));
    let parts = parts.filter(|items| !items.is_empty()).unwrap_or_else(|| {
        vec![PromptPart::Text {
            text: prompt.clone(),
        }]
    });
    validate_prompt_parts(&parts)?;
    let has_attachment = parts
        .iter()
        .any(|part| matches!(part, PromptPart::Image { .. } | PromptPart::File { .. }));
    if prompt.trim().is_empty() && !has_attachment {
        return Err("prompt must contain text, an image, or a text file".into());
    }
    let parts_json = parts
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("failed to encode prompt parts: {error}"))?;
    let provider_config = runtime_provider_config(&provider, &selected_model, api_key, 120_000);

    // Chosen before `params` is built, because building it moves `delivery` into the
    // JSON-RPC params.
    let request_timeout = run_start_timeout(delivery.as_deref());

    let mut params = json!({
        "runId": run_id,
        "sessionId": session_id,
        "prompt": prompt,
        "messageId": message_id,
        "parts": parts_json,
        "delivery": delivery.unwrap_or_else(|| "async".into()),
        "resume": resume.unwrap_or(true),
        "providerConfig": provider_config,
    });
    if let Some(workspace_id) = workspace_id.as_deref() {
        params["workspaceId"] = json!(workspace_id);
    }
    if let Some(server_id) = focus_server_id.as_deref() {
        let server = state
            .database
            .servers()
            .get(server_id)
            .map_err(|error| error.to_string())?;
        let mut target = json!({
            "host": "remote",
            "serverId": server.id,
            "environment": server.metadata.environment,
        });
        if let Some(workspace_id) = workspace_id.as_deref() {
            target["workspaceId"] = json!(workspace_id);
        }
        params["focusServerId"] = json!(server_id);
        params["target"] = target;
    }
    if let Some(permission_mode) = permission_mode {
        params["permissionMode"] = json!(permission_mode);
    }
    if let Some(mode) = mode {
        params["mode"] = json!(mode);
    }
    if let Some(policy_id) = policy_id.as_deref() {
        params["policyId"] = json!(policy_id);
    }
    let response = state
        .supervisor
        .request("agent.run.start", params, request_timeout)
        .await
        .map_err(|error| error.to_string())?;
    let returned_run_id = response
        .get("runId")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "agent sidecar returned an invalid run.start response".to_string())?;
    if returned_run_id != run_id {
        return Err("agent sidecar returned a different run id".into());
    }
    // The sidecar always answers with `started`; a response without it is a contract
    // drift, and guessing `true` would make the UI wait for events of a run that never
    // began.
    let started = response
        .get("started")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| "agent sidecar returned an invalid run.start response".to_string())?;
    let duplicate = response
        .get("duplicate")
        .and_then(serde_json::Value::as_bool);
    let resumed = response.get("resumed").and_then(serde_json::Value::as_bool);
    // Only a `sync` request gets a result, and only a `sync` request waited for one. The
    // previous code discarded this field, which made the 16-minute wait above pointless:
    // the caller asked for the outcome and was handed an identity instead.
    let result = response.get("result").cloned();
    Ok(RunStartResponse {
        run_id,
        started,
        duplicate,
        resumed,
        result,
    })
}

#[tauri::command]
pub async fn agent_run_stop(
    state: State<'_, AppState>,
    run_id: String,
) -> Result<RunStopResponse, String> {
    let response = state
        .supervisor
        .request(
            "agent.run.stop",
            json!({ "runId": run_id }),
            std::time::Duration::from_secs(10),
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(RunStopResponse {
        stopped: response
            .get("stopped")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| "agent sidecar returned an invalid run.stop response".to_string())?,
    })
}

#[tauri::command]
pub async fn agent_approval_respond(
    state: State<'_, AppState>,
    approval_id: String,
    run_id: String,
    decision: String,
) -> Result<ApprovalRespondResponse, String> {
    let response = state
        .supervisor
        .request(
            "agent.approval.respond",
            json!({
                "approvalId": approval_id,
                "runId": run_id,
                "decision": decision,
                "respondedAt": yukinal_core::sidecar::iso8601_now(),
            }),
            std::time::Duration::from_secs(10),
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(ApprovalRespondResponse {
        accepted: response
            .get("accepted")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| "agent sidecar returned an invalid approval response".to_string())?,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        run_start_timeout, validate_prompt_parts, PromptPart, RunStartResponse, MAX_AUDIO_BYTES,
        MAX_FILE_BYTES, MAX_IMAGE_BYTES,
    };
    use std::time::Duration;

    /// The sidecar's own wall-clock bound for one run (`maxRunMs`, 15 minutes by default
    /// in `apps/agent/src/config.ts`). Rust cannot read that value; this constant is the
    /// coupling between the two, stated where a test can hold it.
    const SIDECAR_DEFAULT_MAX_RUN_MS: Duration = Duration::from_secs(15 * 60);

    const FIXTURE: &str =
        include_str!("../../../../../packages/shared/fixtures/ipc/agent_run_start.json");

    /// The `delivery: "sync"` shape, gated by the same fixture the TypeScript contract test
    /// parses. Both halves matter: the async fixture pins that `result` stays *absent* when
    /// there is no run outcome, this one pins that it survives the trip when there is.
    const SYNC_FIXTURE: &str =
        include_str!("../../../../../packages/shared/fixtures/ipc/agent_run_start_sync.json");

    #[test]
    fn admission_is_not_waited_for_like_a_sync_run() {
        assert_eq!(run_start_timeout(None), Duration::from_secs(10));
        assert_eq!(run_start_timeout(Some("async")), Duration::from_secs(10));
        assert_eq!(
            run_start_timeout(Some("sync")),
            Duration::from_secs(16 * 60)
        );
    }

    #[test]
    fn the_sync_wait_outlasts_the_run_it_waits_for() {
        // A sync response *is* the run's result. A bound inside the sidecar's own would
        // abandon a run that is still allowed to finish, and answer the caller with a
        // transport timeout instead of the answer it asked for.
        assert!(run_start_timeout(Some("sync")) > SIDECAR_DEFAULT_MAX_RUN_MS);
    }

    #[test]
    fn an_unrecognised_delivery_falls_back_to_async() {
        // Rust does not validate `delivery` (the sidecar's schema does), so an unknown
        // value must fall back to the short wait rather than holding the request open for
        // sixteen minutes.
        assert_eq!(run_start_timeout(Some("stream")), Duration::from_secs(10));
    }

    #[test]
    fn the_start_response_matches_the_shared_fixture() {
        let actual = serde_json::to_value(RunStartResponse {
            run_id: "run_20260101".into(),
            started: true,
            duplicate: None,
            resumed: None,
            result: None,
        })
        .expect("serialize");
        let expected: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture");
        assert_eq!(actual, expected);
    }

    #[test]
    fn a_sync_start_response_matches_the_shared_fixture() {
        let expected: serde_json::Value = serde_json::from_str(SYNC_FIXTURE).expect("fixture");
        let actual = serde_json::to_value(RunStartResponse {
            run_id: "run_20260101".into(),
            started: true,
            duplicate: None,
            resumed: None,
            result: expected.get("result").cloned(),
        })
        .expect("serialize");
        assert_eq!(actual, expected);
        assert!(
            actual.get("result").is_some(),
            "a sync response without its result is an identity, not an outcome"
        );
    }

    #[test]
    fn admission_flags_survive_the_ipc_boundary() {
        let duplicate = serde_json::to_value(RunStartResponse {
            run_id: "run_existing".into(),
            started: false,
            duplicate: Some(true),
            resumed: None,
            result: None,
        })
        .expect("serialize duplicate");
        assert_eq!(duplicate["duplicate"], serde_json::json!(true));
        assert!(duplicate.get("resumed").is_none());

        let resumed = serde_json::to_value(RunStartResponse {
            run_id: "run_admitted".into(),
            started: true,
            duplicate: None,
            resumed: Some(true),
            result: None,
        })
        .expect("serialize resumed");
        assert_eq!(resumed["resumed"], serde_json::json!(true));
        assert!(resumed.get("duplicate").is_none());
    }

    #[test]
    fn prompt_validation_rejects_bad_media_and_unbounded_inline_data() {
        assert!(validate_prompt_parts(&[PromptPart::Image {
            media_type: "image/png".into(),
            data: "aGVsbG8=".into(),
            name: Some("screen.png".into()),
        }])
        .is_ok());
        assert!(validate_prompt_parts(&[PromptPart::Image {
            media_type: "image/svg+xml".into(),
            data: "aGVsbG8=".into(),
            name: None,
        }])
        .is_err());
        assert!(validate_prompt_parts(&[PromptPart::Image {
            media_type: "image/png".into(),
            data: "not base64".into(),
            name: None,
        }])
        .is_err());
        assert!(validate_prompt_parts(&[PromptPart::Text {
            text: "x".repeat(100_001)
        }])
        .is_err());

        assert!(validate_prompt_parts(&[PromptPart::File {
            media_type: "text/plain".into(),
            data: "PORT=8080\n".into(),
            name: "app.env".into(),
        }])
        .is_ok());
        assert!(validate_prompt_parts(&[PromptPart::File {
            media_type: "application/pdf".into(),
            data: "%PDF".into(),
            name: "manual.pdf".into(),
        }])
        .is_err());
        assert!(validate_prompt_parts(&[PromptPart::File {
            media_type: "text/plain".into(),
            data: "hello\0world".into(),
            name: "binary.txt".into(),
        }])
        .is_err());
        assert!(validate_prompt_parts(&[PromptPart::File {
            media_type: "text/plain".into(),
            data: "x".repeat(MAX_FILE_BYTES + 1),
            name: "large.txt".into(),
        }])
        .is_err());

        assert!(validate_prompt_parts(&[PromptPart::Document {
            media_type: "application/pdf".into(),
            data: "JVBERi0xLjcK".into(),
            name: "manual.pdf".into(),
        }])
        .is_ok());
        assert!(validate_prompt_parts(&[PromptPart::Document {
            media_type: "application/octet-stream".into(),
            data: "JVBERi0xLjcK".into(),
            name: "manual.pdf".into(),
        }])
        .is_err());
        assert!(validate_prompt_parts(&[PromptPart::Document {
            media_type: "application/pdf".into(),
            data: "not base64".into(),
            name: "manual.pdf".into(),
        }])
        .is_err());

        // 音频：四种格式、可选名字、与图片/PDF 共用的总预算。
        assert!(validate_prompt_parts(&[PromptPart::Audio {
            media_type: "audio/wav".into(),
            data: "UklGRgAAAABXQVZFAA==".into(),
            name: Some("note.wav".into()),
        }])
        .is_ok());
        assert!(
            validate_prompt_parts(&[PromptPart::Audio {
                media_type: "audio/ogg".into(),
                data: "T2dnUwAA".into(),
                name: None,
            }])
            .is_ok(),
            "a clip needs no visible name, the same way an image does not"
        );
        for (media_type, data, name) in [
            ("audio/aac", "UklGRgAAAABXQVZFAA==", Some("note.aac")),
            ("audio/wav", "not base64", Some("note.wav")),
            ("audio/wav", "UklGRgAAAABXQVZFAA==", Some("../note.wav")),
        ] {
            assert!(
                validate_prompt_parts(&[PromptPart::Audio {
                    media_type: media_type.into(),
                    data: data.into(),
                    name: name.map(str::to_string),
                }])
                .is_err(),
                "{media_type} / {data} / {name:?} must be refused"
            );
        }
        assert!(validate_prompt_parts(&[
            PromptPart::Audio {
                media_type: "audio/wav".into(),
                data: "UklGRgAAAABXQVZFAA==".into(),
                name: None,
            },
            PromptPart::Audio {
                media_type: "audio/wav".into(),
                data: "UklGRgAAAABXQVZFAA==".into(),
                name: None,
            },
            PromptPart::Audio {
                media_type: "audio/wav".into(),
                data: "UklGRgAAAABXQVZFAA==".into(),
                name: None,
            },
        ])
        .is_err());
        // 单段上限是换算后的字节数，而不是 base64 字符数：这份 ~4.05 MiB 的字节必须被拒绝。
        let oversize = "A".repeat((MAX_AUDIO_BYTES / 3 + 1) * 4);
        assert!(validate_prompt_parts(&[PromptPart::Audio {
            media_type: "audio/flac".into(),
            data: oversize.clone(),
            name: None,
        }])
        .is_err());
        // 与图片共用一个总预算：一张贴着单图上限的图片再加一段超过 1 MiB 的音频就超了，
        // 而两者各自都还在自己的上限之内。
        // `(MAX_IMAGE_BYTES / 3) * 4` 个 base64 字符解码出 4_194_303 字节：刚好在单图上限
        // 之内，而且长度天然是 4 的倍数（base64 的形状要求）。
        let biggest_image = "A".repeat((MAX_IMAGE_BYTES / 3) * 4);
        let bulky_clip = "A".repeat((1_200_000 / 3 + 1) * 4);
        assert!(validate_prompt_parts(&[PromptPart::Image {
            media_type: "image/png".into(),
            data: biggest_image.clone(),
            name: None,
        }])
        .is_ok());
        assert!(validate_prompt_parts(&[
            PromptPart::Image {
                media_type: "image/png".into(),
                data: biggest_image,
                name: None,
            },
            PromptPart::Audio {
                media_type: "audio/wav".into(),
                data: bulky_clip,
                name: None,
            }
        ])
        .is_err());
    }
}

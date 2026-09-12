//! Agent run commands: the UI sends a prompt; Rust resolves the provider +
//! credential (SQLite row + OS keychain), forwards `agent.run.start` to the
//! sidecar, and streams every observable step back as Tauri events.
//!
//! The sidecar never sees a key until this call: material rides only on the
//! transient JSON-RPC params (ADR 0001/0006; resolve secrets at the point of use).

use std::time::Duration;

use serde::Serialize;
use serde_json::json;
use tauri::State;

use crate::commands::provider::resolve_api_key;
use crate::state::AppState;
use yukinal_core::provider::runtime_provider_config;
use yukinal_database::models::AiProviderConfig;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptPart {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStartResponse {
    pub run_id: String,
    /// Whether *this* call began execution, taken from the sidecar's answer rather than
    /// assumed. `false` means the message was admitted without being executed
    /// (`resume: false`) or that a retry hit a run that already exists — in both cases
    /// no `agent.*` event for this call may be expected.
    pub started: bool,
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStopResponse {
    pub stopped: bool,
}

#[derive(Debug, Serialize)]
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

    // Millisecond timestamps can collide when two submissions arrive in the
    // same tick; use the process-wide opaque id generator instead.
    let run_id = run_id
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| crate::commands::server::next_id("run"));
    let message_id = message_id.unwrap_or_else(|| format!("msg_{run_id}"));
    let parts = parts.filter(|items| !items.is_empty()).unwrap_or_else(|| {
        vec![PromptPart {
            kind: "text".into(),
            text: prompt.clone(),
        }]
    });
    let provider_config = runtime_provider_config(&provider, &selected_model, api_key, 120_000);

    // Chosen before `params` is built, because building it moves `delivery` into the
    // JSON-RPC params.
    let request_timeout = run_start_timeout(delivery.as_deref());

    let mut params = json!({
        "runId": run_id,
        "sessionId": session_id,
        "prompt": prompt,
        "messageId": message_id,
        "parts": parts
            .into_iter()
            .map(|part| json!({ "type": part.kind, "text": part.text }))
            .collect::<Vec<_>>(),
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
    // Only a `sync` request gets a result, and only a `sync` request waited for one. The
    // previous code discarded this field, which made the 16-minute wait above pointless:
    // the caller asked for the outcome and was handed an identity instead.
    let result = response.get("result").cloned();
    Ok(RunStartResponse {
        run_id,
        started,
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
    use super::{run_start_timeout, RunStartResponse};
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
            result: expected.get("result").cloned(),
        })
        .expect("serialize");
        assert_eq!(actual, expected);
        assert!(
            actual.get("result").is_some(),
            "a sync response without its result is an identity, not an outcome"
        );
    }
}

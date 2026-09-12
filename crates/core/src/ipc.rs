//! IPC response surface of the native core.
//!
//! Every struct here mirrors a response shape of `IpcCommandMap` in
//! `@yukinal/shared` (`packages/shared/src/ipc/index.ts`): same command name, same
//! camelCase field names, same values. Tauri commands are *thin marshallers* over
//! these types; the desktop crate must not define its own response structs or the
//! two sides can drift without a compiler noticing.
//!
//! The contract tests in this module are the Rust half of a two-sided gate: the
//! canonical JSON lives in `packages/shared/fixtures/ipc/` and is compiled in via
//! `include_str!`, and the TypeScript half (`schemas/ipc.test.ts`) parses the very
//! same files. If serde emits anything a zod schema would reject, one of the two
//! sides goes red.

use serde::Serialize;

use crate::supervisor::StartOutcome;

/// `core_ping` response: proves the IPC round trip without pretending to work.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PingResponse {
    pub version: &'static str,
    pub os: &'static str,
}

/// `agent_spawn` response. `entry` reports *what actually started* (resolution order
/// lives in `SidecarConfig`, deliberately made visible to the UI).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSpawnResponse {
    pub pid: u32,
    pub protocol_version: String,
    pub agent_version: String,
    pub entry: String,
    pub tool_count: usize,
    pub already_running: bool,
}

/// The one place `RuntimeInfo` becomes the wire shape.
///
/// This mapping used to be written out field by field in
/// `apps/desktop/src-tauri/src/commands/mod.rs`, which is exactly what this module's
/// header forbids ("the desktop crate must not define its own response structs").
/// A six-field hand copy is not a style problem: adding a field to `RuntimeInfo` and
/// forgetting the copy produces a response that still compiles, still serialises, and
/// is silently missing the new field. Living here, a new field must be threaded
/// through on purpose.
///
/// Note what is deliberately dropped: `RuntimeInfo::started_at` is not part of the
/// spawn response. The contract fixture (`agent_spawn.json`) has no `startedAt`, and
/// `agent_status` is where a caller learns when the runtime started. Both facts are
/// pinned by the fixture tests below.
impl From<&StartOutcome> for AgentSpawnResponse {
    fn from(outcome: &StartOutcome) -> Self {
        Self {
            pid: outcome.runtime.pid,
            protocol_version: outcome.runtime.protocol_version.clone(),
            agent_version: outcome.runtime.agent_version.clone(),
            entry: outcome.runtime.entry.clone(),
            tool_count: outcome.runtime.tool_count,
            already_running: outcome.already_running,
        }
    }
}

/// `agent_kill` response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentKillResponse {
    pub killed: bool,
}

/// `agent_logs` response: bounded sidecar stderr tail, newest last.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentLogsResponse {
    pub lines: Vec<String>,
    pub capacity: usize,
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::supervisor::{ExitRecord, SupervisorStatus};

    /// `include_str!` paths are relative to this file: crates/core/src -> repo root.
    const FIXTURE_CORE_PING: &str =
        include_str!("../../../packages/shared/fixtures/ipc/core_ping.json");
    const FIXTURE_AGENT_SPAWN: &str =
        include_str!("../../../packages/shared/fixtures/ipc/agent_spawn.json");
    const FIXTURE_AGENT_KILL: &str =
        include_str!("../../../packages/shared/fixtures/ipc/agent_kill.json");
    const FIXTURE_AGENT_LOGS: &str =
        include_str!("../../../packages/shared/fixtures/ipc/agent_logs.json");
    const FIXTURE_AGENT_STATUS: &str =
        include_str!("../../../packages/shared/fixtures/ipc/agent_status.json");
    const FIXTURE_AGENT_STATUS_EXITED: &str =
        include_str!("../../../packages/shared/fixtures/ipc/agent_status_exited.json");

    fn fixture(raw: &str) -> Value {
        serde_json::from_str(raw).expect("contract fixture must be valid JSON")
    }

    #[test]
    fn core_ping_serializes_to_the_contract_fixture() {
        let actual = serde_json::to_value(PingResponse {
            version: "0.1.0",
            os: "windows",
        })
        .expect("serialize");
        assert_eq!(actual, fixture(FIXTURE_CORE_PING));
        assert_eq!(
            actual,
            serde_json::json!({ "version": "0.1.0", "os": "windows" })
        );
    }

    #[test]
    fn agent_spawn_serializes_to_the_contract_fixture() {
        let actual = serde_json::to_value(AgentSpawnResponse {
            pid: 25_980,
            protocol_version: "1.0".into(),
            agent_version: "0.1.0".into(),
            entry: "apps/agent/dist/index.js".into(),
            tool_count: 1,
            already_running: false,
        })
        .expect("serialize");
        assert_eq!(actual, fixture(FIXTURE_AGENT_SPAWN));
        // pid must stay a JSON number; a stringified pid breaks every UI consumer.
        assert_eq!(actual.get("pid").and_then(Value::as_u64), Some(25_980));
    }

    /// The `From` impl must carry every contract field across, and must not invent one.
    ///
    /// Asserted against the fixture rather than against literal values, because the
    /// fixture is the thing the TypeScript gate also parses — so this test fails if the
    /// mapping and the shared contract disagree, not merely if the mapping changes.
    #[test]
    fn spawn_response_from_start_outcome_matches_the_contract_fixture() {
        use crate::supervisor::RuntimeInfo;

        let outcome = StartOutcome {
            runtime: RuntimeInfo {
                pid: 25_980,
                protocol_version: "1.0".into(),
                agent_version: "0.1.0".into(),
                entry: "apps/agent/dist/index.js".into(),
                tool_count: 1,
                started_at: "2026-01-01T00:00:00Z".into(),
            },
            already_running: false,
        };

        let actual = serde_json::to_value(AgentSpawnResponse::from(&outcome)).expect("serialize");
        assert_eq!(
            actual,
            fixture(FIXTURE_AGENT_SPAWN),
            "From<&StartOutcome> 与共享契约的 agent_spawn 形状不一致",
        );

        // `startedAt` 有意不在这个形状里：它的归属是 agent_status。若有人「顺手」把
        // RuntimeInfo 的新字段全量搬过来，这一条会失败，并指出该去哪看。
        assert!(
            actual.get("startedAt").is_none(),
            "agent_spawn 契约里没有 startedAt；启动时间由 agent_status 报告",
        );
    }

    #[test]
    fn agent_kill_serializes_to_the_contract_fixture() {
        let actual = serde_json::to_value(AgentKillResponse { killed: true }).expect("serialize");
        assert_eq!(actual, fixture(FIXTURE_AGENT_KILL));
    }

    #[test]
    fn agent_logs_serializes_to_the_contract_fixture() {
        let actual = serde_json::to_value(AgentLogsResponse {
            lines: vec![
                "[agent] starting".into(),
                "[agent] protocol 1.0 ready".into(),
            ],
            capacity: 200,
        })
        .expect("serialize");
        assert_eq!(actual, fixture(FIXTURE_AGENT_LOGS));
        assert_eq!(
            actual,
            serde_json::json!({ "lines": ["[agent] starting", "[agent] protocol 1.0 ready"], "capacity": 200 })
        );
    }

    #[test]
    fn running_agent_status_serializes_to_the_contract_fixture() {
        let actual = serde_json::to_value(SupervisorStatus {
            running: true,
            pid: Some(25_980),
            protocol_version: Some("1.0".into()),
            agent_version: Some("0.1.0".into()),
            tool_count: Some(1),
            entry: Some("apps/agent/dist/index.js".into()),
            started_at: Some("2026-01-01T00:00:00Z".into()),
            last_exit: None,
            restart: None,
        })
        .expect("serialize");
        assert_eq!(actual, fixture(FIXTURE_AGENT_STATUS));
    }

    #[test]
    fn exited_agent_status_serializes_to_the_contract_fixture() {
        let actual = serde_json::to_value(SupervisorStatus {
            running: false,
            pid: None,
            protocol_version: None,
            agent_version: None,
            tool_count: None,
            entry: None,
            started_at: None,
            last_exit: Some(ExitRecord {
                code: Some(1),
                signal: None,
                at: "2026-01-01T00:05:00Z".into(),
            }),
            restart: None,
        })
        .expect("serialize");
        assert_eq!(actual, fixture(FIXTURE_AGENT_STATUS_EXITED));
    }
}

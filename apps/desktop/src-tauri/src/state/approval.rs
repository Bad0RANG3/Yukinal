//! Rust-owned, one-time tickets for exact `server.exec` approvals.
//!
//! Approval events are observations from the sidecar, but the UI cannot create a
//! ticket: this ledger records the event in host memory, moves it to an approved
//! state only after the host receives an accepted user response, and consumes it
//! once against the full host tool request before opening an SSH session.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Notify;

const SERVER_EXEC: &str = "server.exec";
const MAX_TICKETS: usize = 256;
const MAX_APPROVAL_TTL_SECONDS: u64 = 180;

#[derive(Debug, Default)]
pub(crate) struct HostApprovalLedger {
    entries: Mutex<HashMap<String, ApprovalEntry>>,
}

#[derive(Debug)]
struct ApprovalEntry {
    binding: ApprovalBinding,
    phase: ApprovalPhase,
    changed: Arc<Notify>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalPhase {
    Pending,
    Responding,
    Approved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApprovalBinding {
    approval_id: String,
    run_id: String,
    trace_id: String,
    call_id: String,
    tool_name: String,
    input_fingerprint: String,
    target_host: String,
    server_id: String,
    workspace_id: Option<String>,
    environment: String,
    task_id: String,
    plan_id: String,
    plan_step_id: String,
    evidence_ids: Option<Vec<String>>,
    expires_at: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WaitingApprovalEvent {
    #[serde(rename = "type")]
    event_type: String,
    run_id: String,
    task_id: Option<String>,
    approval: ApprovalEvent,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalEvent {
    approval_id: String,
    run_id: String,
    trace_id: String,
    call_id: String,
    tool_name: String,
    input_fingerprint: String,
    target: ApprovalTarget,
    expires_at: String,
    plan_id: Option<String>,
    plan_step_id: Option<String>,
    evidence_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalTarget {
    host: String,
    server_id: Option<String>,
    workspace_id: Option<String>,
    environment: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UserDecision {
    ApproveOnce,
    Reject,
}

#[derive(Debug)]
pub(crate) struct ApprovalResponseToken {
    approval_id: String,
    decision: UserDecision,
}

/// Fields supplied by the host-tool request and compared with the approval ticket.
/// The command input itself is represented by a host-recomputed SHA-256 fingerprint.
pub(crate) struct ServerExecExecutionBinding<'a> {
    pub approval_id: &'a str,
    pub run_id: Option<&'a str>,
    pub trace_id: &'a str,
    pub call_id: &'a str,
    pub tool_name: &'a str,
    pub input_fingerprint: &'a str,
    pub target_host: &'a str,
    pub server_id: Option<&'a str>,
    pub workspace_id: Option<&'a str>,
    pub environment: &'a str,
    pub task_id: Option<&'a str>,
    pub plan_id: Option<&'a str>,
    pub plan_step_id: Option<&'a str>,
    pub evidence_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServerExecTicketError {
    Missing,
    Expired,
    Mismatch,
    NotApproved,
}

impl HostApprovalLedger {
    /// Capture only well-formed server command approvals. The input itself stays
    /// redacted in the stream; its fingerprint binds the eventual host request to
    /// the exact original input without copying command text into this ledger.
    pub(crate) fn record_waiting_event(&self, params: &Value, now: u64) -> Result<bool, String> {
        let event = serde_json::from_value::<WaitingApprovalEvent>(params.clone())
            .map_err(|_| "approval event is missing host binding fields".to_string())?;
        if event.event_type != "agent.waiting_approval"
            || event.approval.tool_name != SERVER_EXEC
            || event.approval.run_id != event.run_id
            || event.run_id.trim().is_empty()
            || event.run_id.len() > 256
            || event.approval.approval_id.trim().is_empty()
            || event.approval.approval_id.len() > 256
            || event.approval.trace_id.trim().is_empty()
            || event.approval.trace_id.len() > 256
            || event.approval.call_id.trim().is_empty()
            || event.approval.call_id.len() > 256
            || !is_sha256_hex(&event.approval.input_fingerprint)
            || event.approval.target.host != "remote"
            || !event
                .approval
                .target
                .server_id
                .as_deref()
                .is_some_and(|server_id| server_id.starts_with("srv_") && server_id.len() <= 256)
            || event.approval.target.environment.trim().is_empty()
            || event.approval.target.environment.len() > 64
        {
            return Err("approval event does not describe a valid server.exec binding".into());
        }
        let Some(task_id) = event.task_id.filter(|value| !value.trim().is_empty()) else {
            return Err("server.exec approval requires a durable task".into());
        };
        let Some(plan_id) = event
            .approval
            .plan_id
            .filter(|value| !value.trim().is_empty())
        else {
            return Err("server.exec approval requires an approved plan".into());
        };
        let Some(plan_step_id) = event
            .approval
            .plan_step_id
            .filter(|value| !value.trim().is_empty())
        else {
            return Err("server.exec approval requires an approved plan step".into());
        };
        let expires_at = yukinal_time::parse_iso8601_utc(&event.approval.expires_at)
            .ok_or_else(|| "server.exec approval expiry is invalid".to_string())?;
        if expires_at <= now || expires_at.saturating_sub(now) > MAX_APPROVAL_TTL_SECONDS {
            return Err("server.exec approval is expired or exceeds the host TTL limit".into());
        }
        let Some(server_id) = event.approval.target.server_id else {
            return Err("server.exec approval requires a resolved server".into());
        };

        let binding = ApprovalBinding {
            approval_id: event.approval.approval_id,
            run_id: event.run_id,
            trace_id: event.approval.trace_id,
            call_id: event.approval.call_id,
            tool_name: event.approval.tool_name,
            input_fingerprint: event.approval.input_fingerprint,
            target_host: event.approval.target.host,
            server_id,
            workspace_id: event.approval.target.workspace_id,
            environment: event.approval.target.environment,
            task_id,
            plan_id,
            plan_step_id,
            evidence_ids: event.approval.evidence_ids,
            expires_at,
        };

        let mut entries = self
            .entries
            .lock()
            .map_err(|_| "server.exec approval ledger is poisoned".to_string())?;
        entries.retain(|_, entry| entry.binding.expires_at > now);
        if let Some(existing) = entries.get(&binding.approval_id) {
            return if existing.binding == binding {
                Ok(true)
            } else {
                Err("approval id is already bound to another server.exec request".into())
            };
        }
        if entries.len() >= MAX_TICKETS {
            return Err("server.exec approval ledger is full".into());
        }
        entries.insert(
            binding.approval_id.clone(),
            ApprovalEntry {
                binding,
                phase: ApprovalPhase::Pending,
                changed: Arc::new(Notify::new()),
            },
        );
        Ok(true)
    }

    /// Mark a known server.exec ticket as waiting for the matching sidecar ack.
    /// `None` means the approval belongs to another tool, which keeps existing
    /// approvals on their current round trip without granting this host ticket.
    pub(crate) fn begin_response(
        &self,
        approval_id: &str,
        run_id: &str,
        decision: &str,
        now: u64,
    ) -> Result<Option<ApprovalResponseToken>, String> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| "server.exec approval ledger is poisoned".to_string())?;
        let Some(entry) = entries.get_mut(approval_id) else {
            return Ok(None);
        };
        if entry.binding.expires_at <= now {
            entries.remove(approval_id);
            return Err("server.exec approval has expired".into());
        }
        if entry.binding.run_id != run_id {
            return Err("server.exec approval run id does not match".into());
        }
        if entry.phase != ApprovalPhase::Pending {
            return Err("server.exec approval was already answered".into());
        }
        let decision = match decision {
            "approve_once" => UserDecision::ApproveOnce,
            "reject" => UserDecision::Reject,
            "approve_session" => {
                return Err(
                    "server.exec approvals are per call and cannot be session grants".into(),
                )
            }
            _ => return Err("approval decision is invalid".into()),
        };
        entry.phase = ApprovalPhase::Responding;
        Ok(Some(ApprovalResponseToken {
            approval_id: approval_id.to_string(),
            decision,
        }))
    }

    /// Complete the approval response. A ticket becomes executable only when the
    /// sidecar accepted this response; failed or rejected responses remove it.
    pub(crate) fn finish_response(
        &self,
        token: ApprovalResponseToken,
        accepted: bool,
        now: u64,
    ) -> Result<(), String> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| "server.exec approval ledger is poisoned".to_string())?;
        let Some(entry) = entries.get_mut(&token.approval_id) else {
            return Ok(());
        };
        if entry.phase != ApprovalPhase::Responding {
            return Err("server.exec approval response is no longer active".into());
        }
        let approved = accepted
            && token.decision == UserDecision::ApproveOnce
            && entry.binding.expires_at > now;
        let changed = entry.changed.clone();
        if approved {
            entry.phase = ApprovalPhase::Approved;
        } else {
            entries.remove(&token.approval_id);
        }
        drop(entries);
        changed.notify_one();
        Ok(())
    }

    /// Consume the ticket exactly once. A request racing the approval RPC waits
    /// for its host-side acknowledgement; a mismatch burns the approved ticket.
    pub(crate) async fn consume(
        &self,
        request: ServerExecExecutionBinding<'_>,
        now: impl Fn() -> u64,
    ) -> Result<(), ServerExecTicketError> {
        loop {
            let wait_for_response = {
                let mut entries = self
                    .entries
                    .lock()
                    .map_err(|_| ServerExecTicketError::Missing)?;
                let Some(entry) = entries.get(request.approval_id) else {
                    return Err(ServerExecTicketError::Missing);
                };
                if entry.binding.expires_at <= now() {
                    entries.remove(request.approval_id);
                    return Err(ServerExecTicketError::Expired);
                }
                match entry.phase {
                    ApprovalPhase::Pending => return Err(ServerExecTicketError::NotApproved),
                    ApprovalPhase::Responding => Some(entry.changed.clone()),
                    ApprovalPhase::Approved => {
                        let entry = entries
                            .remove(request.approval_id)
                            .expect("entry was checked under the same lock");
                        return if entry.binding.matches(&request) {
                            Ok(())
                        } else {
                            Err(ServerExecTicketError::Mismatch)
                        };
                    }
                }
            };
            if let Some(changed) = wait_for_response {
                tokio::select! {
                    _ = changed.notified() => {},
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                }
            }
        }
    }
}

impl ApprovalBinding {
    fn matches(&self, request: &ServerExecExecutionBinding<'_>) -> bool {
        self.approval_id == request.approval_id
            && request.run_id == Some(self.run_id.as_str())
            && self.trace_id == request.trace_id
            && self.call_id == request.call_id
            && self.tool_name == request.tool_name
            && request.tool_name == SERVER_EXEC
            && self.input_fingerprint == request.input_fingerprint
            && self.target_host == request.target_host
            && request.server_id == Some(self.server_id.as_str())
            && self.workspace_id.as_deref() == request.workspace_id
            && self.environment == request.environment
            && request.task_id == Some(self.task_id.as_str())
            && request.plan_id == Some(self.plan_id.as_str())
            && request.plan_step_id == Some(self.plan_step_id.as_str())
            && self.evidence_ids.as_deref() == request.evidence_ids.as_deref()
    }
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::{HostApprovalLedger, ServerExecExecutionBinding, ServerExecTicketError};
    use serde_json::json;
    use std::time::Duration;

    const NOW: u64 = 1_800_000_000;
    const EXPIRY: &str = "2027-01-15T08:02:00Z";
    const INPUT_FINGERPRINT: &str =
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn waiting_event() -> serde_json::Value {
        json!({
            "type": "agent.waiting_approval",
            "runId": "run_1",
            "taskId": "task_1",
            "approval": {
                "approvalId": "apr_1",
                "runId": "run_1",
                "traceId": "trace_1",
                "callId": "call_1",
                "toolName": "server.exec",
                "input": { "command": "systemctl restart api" },
                "inputFingerprint": INPUT_FINGERPRINT,
                "target": {
                    "host": "remote",
                    "serverId": "srv_1",
                    "workspaceId": "workspace_1",
                    "environment": "staging"
                },
                "planId": "plan_1",
                "planStepId": "step_1",
                "evidenceIds": ["ev_1"],
                "expiresAt": EXPIRY
            },
            "at": "2027-01-15T08:00:00Z"
        })
    }

    fn execution_binding<'a>() -> ServerExecExecutionBinding<'a> {
        ServerExecExecutionBinding {
            approval_id: "apr_1",
            run_id: Some("run_1"),
            trace_id: "trace_1",
            call_id: "call_1",
            tool_name: "server.exec",
            input_fingerprint: INPUT_FINGERPRINT,
            target_host: "remote",
            server_id: Some("srv_1"),
            workspace_id: Some("workspace_1"),
            environment: "staging",
            task_id: Some("task_1"),
            plan_id: Some("plan_1"),
            plan_step_id: Some("step_1"),
            evidence_ids: Some(vec!["ev_1".to_string()]),
        }
    }

    fn approve_once(ledger: &HostApprovalLedger) {
        let token = ledger
            .begin_response("apr_1", "run_1", "approve_once", NOW)
            .expect("begin response")
            .expect("server.exec response token");
        ledger
            .finish_response(token, true, NOW)
            .expect("finish accepted response");
    }

    #[tokio::test]
    async fn user_approval_creates_an_exact_ticket_that_is_consumed_once() {
        let ledger = HostApprovalLedger::default();
        assert!(ledger
            .record_waiting_event(&waiting_event(), NOW)
            .expect("record waiting approval"));
        approve_once(&ledger);

        assert_eq!(ledger.consume(execution_binding(), || NOW).await, Ok(()));
        assert_eq!(
            ledger.consume(execution_binding(), || NOW).await,
            Err(ServerExecTicketError::Missing)
        );
    }

    #[tokio::test]
    async fn tickets_fail_closed_for_forged_unapproved_and_replayed_requests() {
        let ledger = HostApprovalLedger::default();
        assert_eq!(
            ledger.consume(execution_binding(), || NOW).await,
            Err(ServerExecTicketError::Missing)
        );

        ledger
            .record_waiting_event(&waiting_event(), NOW)
            .expect("record waiting approval");
        assert_eq!(
            ledger.consume(execution_binding(), || NOW).await,
            Err(ServerExecTicketError::NotApproved)
        );
        approve_once(&ledger);
        assert_eq!(
            ledger
                .consume(
                    ServerExecExecutionBinding {
                        approval_id: "apr_forged",
                        ..execution_binding()
                    },
                    || NOW,
                )
                .await,
            Err(ServerExecTicketError::Missing)
        );
        assert_eq!(ledger.consume(execution_binding(), || NOW).await, Ok(()));
        assert_eq!(
            ledger.consume(execution_binding(), || NOW).await,
            Err(ServerExecTicketError::Missing)
        );
    }

    #[tokio::test]
    async fn ticket_is_burned_when_any_approved_binding_field_changes() {
        let ledger = HostApprovalLedger::default();
        ledger
            .record_waiting_event(&waiting_event(), NOW)
            .expect("record waiting approval");
        approve_once(&ledger);

        let mismatches = [
            ServerExecExecutionBinding {
                run_id: Some("run_other"),
                ..execution_binding()
            },
            ServerExecExecutionBinding {
                trace_id: "trace_other",
                ..execution_binding()
            },
            ServerExecExecutionBinding {
                call_id: "call_other",
                ..execution_binding()
            },
            ServerExecExecutionBinding {
                tool_name: "package.install",
                ..execution_binding()
            },
            ServerExecExecutionBinding {
                input_fingerprint:
                    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                ..execution_binding()
            },
            ServerExecExecutionBinding {
                server_id: Some("srv_other"),
                ..execution_binding()
            },
            ServerExecExecutionBinding {
                workspace_id: None,
                ..execution_binding()
            },
            ServerExecExecutionBinding {
                environment: "production",
                ..execution_binding()
            },
            ServerExecExecutionBinding {
                task_id: Some("task_other"),
                ..execution_binding()
            },
            ServerExecExecutionBinding {
                plan_id: Some("plan_other"),
                ..execution_binding()
            },
            ServerExecExecutionBinding {
                plan_step_id: Some("step_other"),
                ..execution_binding()
            },
            ServerExecExecutionBinding {
                evidence_ids: None,
                ..execution_binding()
            },
        ];
        for mismatch in mismatches {
            let isolated = HostApprovalLedger::default();
            isolated
                .record_waiting_event(&waiting_event(), NOW)
                .expect("record waiting approval");
            approve_once(&isolated);
            assert_eq!(
                isolated.consume(mismatch, || NOW).await,
                Err(ServerExecTicketError::Mismatch)
            );
            assert_eq!(
                isolated.consume(execution_binding(), || NOW).await,
                Err(ServerExecTicketError::Missing)
            );
        }
    }

    #[tokio::test]
    async fn expired_or_rejected_approvals_never_issue_a_ticket() {
        let ledger = HostApprovalLedger::default();
        assert!(ledger
            .record_waiting_event(&waiting_event(), NOW)
            .expect("record waiting approval"));
        assert!(ledger
            .begin_response("apr_1", "run_other", "approve_once", NOW)
            .is_err());
        assert!(ledger
            .begin_response("apr_1", "run_1", "approve_session", NOW)
            .is_err());
        let rejected = ledger
            .begin_response("apr_1", "run_1", "reject", NOW)
            .expect("begin rejection")
            .expect("server.exec rejection token");
        ledger
            .finish_response(rejected, true, NOW)
            .expect("finish rejection");
        assert_eq!(
            ledger.consume(execution_binding(), || NOW).await,
            Err(ServerExecTicketError::Missing)
        );

        let mut expired = waiting_event();
        expired["approval"]["expiresAt"] = json!("2027-01-15T07:59:59Z");
        assert!(ledger.record_waiting_event(&expired, NOW).is_err());

        let late = HostApprovalLedger::default();
        late.record_waiting_event(&waiting_event(), NOW)
            .expect("record waiting approval");
        let token = late
            .begin_response("apr_1", "run_1", "approve_once", NOW)
            .expect("begin response")
            .expect("server.exec response token");
        late.finish_response(token, true, NOW)
            .expect("finish accepted response");
        assert_eq!(
            late.consume(execution_binding(), || NOW + 121).await,
            Err(ServerExecTicketError::Expired)
        );

        let unaccepted = HostApprovalLedger::default();
        unaccepted
            .record_waiting_event(&waiting_event(), NOW)
            .expect("record waiting approval");
        let token = unaccepted
            .begin_response("apr_1", "run_1", "approve_once", NOW)
            .expect("begin response")
            .expect("server.exec response token");
        unaccepted
            .finish_response(token, false, NOW)
            .expect("finish unaccepted response");
        assert_eq!(
            unaccepted.consume(execution_binding(), || NOW).await,
            Err(ServerExecTicketError::Missing)
        );
    }

    #[tokio::test]
    async fn execution_waiting_for_the_approval_rpc_resumes_after_host_ack() {
        let ledger = std::sync::Arc::new(HostApprovalLedger::default());
        ledger
            .record_waiting_event(&waiting_event(), NOW)
            .expect("record waiting approval");
        let token = ledger
            .begin_response("apr_1", "run_1", "approve_once", NOW)
            .expect("begin response")
            .expect("server.exec response token");
        let consumer = {
            let ledger = std::sync::Arc::clone(&ledger);
            tokio::spawn(async move { ledger.consume(execution_binding(), || NOW).await })
        };
        let mut consumer = consumer;
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut consumer)
                .await
                .is_err()
        );
        ledger
            .finish_response(token, true, NOW)
            .expect("finish accepted response");
        assert_eq!(consumer.await.expect("join consumer"), Ok(()));
    }
}

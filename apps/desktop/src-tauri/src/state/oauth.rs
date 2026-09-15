//! At most one in-flight OAuth authorization per MCP server, with a way to stop it.
//!
//! The desktop host deliberately has no UI handle inside `commands/mcp/oauth.rs`: the
//! flow is a plain async function, and it is the *command call* that keeps it alive.
//! That is the property this module exists to preserve rather than work around:
//!
//! - the poll loop is awaited inside the command future, so a dropped future (a closed
//!   window, a cancelled client request) stops the polling by construction instead of
//!   leaving a detached task behind;
//! - the entry is removed by [`OAuthFlow`]'s `Drop`, so "is a flow running" cannot go
//!   stale the way a flag cleared on the happy path would;
//! - cancellation travels one way — the UI asks, the loop observes — which is the same
//!   shape as the SSH keyboard-interactive broker next door (`state/auth.rs`), minus the
//!   response half, because a device-code flow has nothing for the user to type back
//!   into Yukinal.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
pub struct OAuthFlowBroker {
    pending: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl OAuthFlowBroker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a flow for `server_id`, or refuse when one is already waiting.
    ///
    /// Refusing rather than replacing is the honest answer: the older flow may already
    /// have opened a browser page and shown a `user_code`, and silently cancelling it
    /// would invalidate what the user is looking at without telling them.
    pub fn begin(&self, server_id: &str) -> Result<OAuthFlow, String> {
        let mut pending = self.pending.lock().expect("OAuth flow map");
        if pending.contains_key(server_id) {
            return Err(format!(
                "MCP 服务器 `{server_id}` 已有一个进行中的授权流程；先取消它，或者等它超时。"
            ));
        }
        let token = CancellationToken::new();
        pending.insert(server_id.to_string(), token.clone());
        Ok(OAuthFlow {
            server_id: server_id.to_string(),
            token,
            broker: self.clone(),
        })
    }

    /// Ask a running flow to stop. `false` means there was nothing to stop.
    pub fn cancel(&self, server_id: &str) -> bool {
        let pending = self.pending.lock().expect("OAuth flow map");
        match pending.get(server_id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }
}

/// One registered flow. Dropping it unregisters the server id.
pub struct OAuthFlow {
    server_id: String,
    token: CancellationToken,
    broker: OAuthFlowBroker,
}

/// Server id only: a cancellation token is not something to print, and a derived `Debug`
/// would put its address in every failed assertion.
impl std::fmt::Debug for OAuthFlow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OAuthFlow")
            .field("server_id", &self.server_id)
            .finish_non_exhaustive()
    }
}

impl OAuthFlow {
    /// The token the flow's wait points observe.
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

impl Drop for OAuthFlow {
    fn drop(&mut self) {
        let mut pending = self.broker.pending.lock().expect("OAuth flow map");
        // Only remove our own registration: a cancel followed by a new flow would
        // otherwise let the old guard delete the new flow's token.
        if pending
            .get(&self.server_id)
            .is_some_and(|token| token == &self.token)
        {
            pending.remove(&self.server_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_flow_for_the_same_server_is_refused_until_the_first_ends() {
        let broker = OAuthFlowBroker::new();
        let first = broker.begin("mcp_1").expect("first flow");

        let refused = broker.begin("mcp_1").expect_err("must refuse");
        assert!(refused.contains("mcp_1"), "{refused}");
        assert!(
            broker.begin("mcp_2").is_ok(),
            "another server is independent"
        );

        drop(first);
        assert!(broker.begin("mcp_1").is_ok(), "the id is free again");
    }

    #[test]
    fn cancelling_marks_the_token_and_reports_whether_anything_was_running() {
        let broker = OAuthFlowBroker::new();
        assert!(!broker.cancel("mcp_1"), "nothing to cancel yet");

        let flow = broker.begin("mcp_1").expect("flow");
        let token = flow.token();
        assert!(!token.is_cancelled());
        assert!(broker.cancel("mcp_1"));
        assert!(token.is_cancelled(), "the flow observes the cancellation");
        // Still `true`: the flow is registered until it unwinds, and it *is* cancelled. The
        // answer is "is it stopped", not "did this call change anything".
        assert!(broker.cancel("mcp_1"));
        assert!(token.is_cancelled());

        drop(flow);
        assert!(
            !broker.cancel("mcp_1"),
            "a finished flow is nothing to cancel"
        );
    }

    #[test]
    fn a_finished_flow_does_not_delete_a_newer_registration() {
        let broker = OAuthFlowBroker::new();
        let first = broker.begin("mcp_1").expect("first flow");
        let second = broker.begin("mcp_1");
        assert!(second.is_err(), "precondition: the id is taken");

        // The guard removes only its own token, so a stale guard cannot free an id that
        // a later flow has since claimed. Simulated by dropping the first guard after the
        // map was hand-edited to a different token.
        let replacement = CancellationToken::new();
        broker
            .pending
            .lock()
            .expect("map")
            .insert("mcp_1".to_string(), replacement.clone());
        drop(first);
        assert!(!replacement.is_cancelled());
        assert!(
            broker.pending.lock().expect("map").contains_key("mcp_1"),
            "the newer registration must survive the older guard's drop"
        );
    }
}

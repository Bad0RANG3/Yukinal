//! One-shot bridge between an SSH keyboard-interactive challenge and the desktop UI.
//!
//! The broker holds no response longer than the single protocol round that requested it.
//! Challenges are ordinary UI events; responses arrive through a command and a oneshot.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{broadcast, oneshot, Mutex};

use yukinal_ssh::{
    Error as SshError, KeyboardInteractiveChallenge, KeyboardInteractiveHandler,
    KeyboardInteractivePrompt,
};

const CHALLENGE_TIMEOUT: Duration = Duration::from_secs(120);
const EVENT_CAPACITY: usize = 32;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthChallengePrompt {
    pub prompt: String,
    pub echo: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthChallengeEvent {
    pub auth_id: String,
    pub server_id: String,
    pub username: String,
    pub host: String,
    pub name: String,
    pub instructions: String,
    pub prompts: Vec<AuthChallengePrompt>,
    pub expires_at: String,
}

enum AuthResponse {
    Responses(Vec<String>),
    Cancelled,
}

struct PendingChallenge {
    prompt_count: usize,
    sender: oneshot::Sender<AuthResponse>,
}

#[derive(Clone)]
pub struct AuthChallengeBroker {
    events: broadcast::Sender<AuthChallengeEvent>,
    pending: Arc<Mutex<HashMap<String, PendingChallenge>>>,
    next_id: Arc<AtomicU64>,
}

impl AuthChallengeBroker {
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            events,
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AuthChallengeEvent> {
        self.events.subscribe()
    }

    pub fn handler(
        &self,
        server_id: String,
        username: String,
        host: String,
    ) -> Arc<dyn KeyboardInteractiveHandler> {
        Arc::new(DesktopKeyboardInteractiveHandler {
            broker: self.clone(),
            server_id,
            username,
            host,
        })
    }

    pub async fn respond(&self, auth_id: &str, responses: Vec<String>) -> bool {
        let mut pending = self.pending.lock().await;
        let Some(challenge) = pending.get(auth_id) else {
            return false;
        };
        if challenge.prompt_count != responses.len() {
            return false;
        }
        let Some(challenge) = pending.remove(auth_id) else {
            return false;
        };
        challenge
            .sender
            .send(AuthResponse::Responses(responses))
            .is_ok()
    }

    pub async fn cancel(&self, auth_id: &str) -> bool {
        let Some(challenge) = self.pending.lock().await.remove(auth_id) else {
            return false;
        };
        challenge.sender.send(AuthResponse::Cancelled).is_ok()
    }

    async fn request(
        &self,
        server_id: &str,
        username: &str,
        host: &str,
        challenge: &KeyboardInteractiveChallenge,
    ) -> Result<Vec<String>, SshError> {
        let auth_id = format!(
            "auth_{}_{}",
            std::process::id(),
            self.next_id.fetch_add(1, Ordering::Relaxed)
        );
        let (sender, receiver) = oneshot::channel();
        let prompts = challenge
            .prompts
            .iter()
            .map(|prompt: &KeyboardInteractivePrompt| AuthChallengePrompt {
                prompt: prompt.prompt.clone(),
                echo: prompt.echo,
            })
            .collect();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let event = AuthChallengeEvent {
            auth_id: auth_id.clone(),
            server_id: server_id.to_string(),
            username: username.to_string(),
            host: host.to_string(),
            name: challenge.name.clone(),
            instructions: challenge.instructions.clone(),
            prompts,
            expires_at: yukinal_core::sidecar::iso8601_utc(
                now.saturating_add(CHALLENGE_TIMEOUT.as_secs()),
            ),
        };
        self.pending.lock().await.insert(
            auth_id.clone(),
            PendingChallenge {
                prompt_count: challenge.prompts.len(),
                sender,
            },
        );
        if self.events.send(event).is_err() {
            self.pending.lock().await.remove(&auth_id);
            return Err(SshError::Authentication(
                "the desktop UI is not listening for SSH authentication challenges".into(),
            ));
        }

        match tokio::time::timeout(CHALLENGE_TIMEOUT, receiver).await {
            Ok(Ok(AuthResponse::Responses(responses))) => Ok(responses),
            Ok(Ok(AuthResponse::Cancelled)) => Err(SshError::Cancelled),
            Ok(Err(_)) | Err(_) => {
                self.pending.lock().await.remove(&auth_id);
                Err(SshError::Authentication(
                    "the SSH second-factor challenge expired or was closed".into(),
                ))
            }
        }
    }
}

struct DesktopKeyboardInteractiveHandler {
    broker: AuthChallengeBroker,
    server_id: String,
    username: String,
    host: String,
}

impl KeyboardInteractiveHandler for DesktopKeyboardInteractiveHandler {
    fn respond<'a>(
        &'a self,
        challenge: &'a KeyboardInteractiveChallenge,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, SshError>> + Send + 'a>> {
        Box::pin(async move {
            self.broker
                .request(&self.server_id, &self.username, &self.host, challenge)
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn challenge() -> KeyboardInteractiveChallenge {
        KeyboardInteractiveChallenge {
            name: "Two-factor authentication".into(),
            instructions: "Enter the code".into(),
            prompts: vec![KeyboardInteractivePrompt {
                prompt: "Code".into(),
                echo: false,
            }],
        }
    }

    #[tokio::test]
    async fn a_challenge_round_trips_one_response_through_the_oneshot() {
        let broker = AuthChallengeBroker::new();
        let handler = broker.handler("srv_1".into(), "deploy".into(), "api.example.test".into());
        let mut events = broker.subscribe();
        let challenge = challenge();
        let responses = tokio::spawn(async move { handler.respond(&challenge).await });

        let event = events.recv().await.expect("challenge event");
        assert_eq!(event.server_id, "srv_1");
        assert_eq!(event.prompts.len(), 1);
        assert!(broker.respond(&event.auth_id, vec!["654321".into()]).await);
        assert_eq!(
            responses.await.expect("join").expect("responses"),
            vec!["654321"]
        );
    }

    #[tokio::test]
    async fn a_response_count_mismatch_is_refused_without_consuming_the_challenge() {
        let broker = AuthChallengeBroker::new();
        let handler = broker.handler("srv_1".into(), "deploy".into(), "api.example.test".into());
        let mut events = broker.subscribe();
        let challenge = challenge();
        let responses = tokio::spawn(async move { handler.respond(&challenge).await });
        let event = events.recv().await.expect("challenge event");

        assert!(!broker.respond(&event.auth_id, vec![]).await);
        assert!(broker.respond(&event.auth_id, vec!["654321".into()]).await);
        assert_eq!(
            responses.await.expect("join").expect("responses"),
            vec!["654321"]
        );
    }

    #[tokio::test]
    async fn cancellation_is_terminal_and_not_translated_into_an_empty_secret() {
        let broker = AuthChallengeBroker::new();
        let handler = broker.handler("srv_1".into(), "deploy".into(), "api.example.test".into());
        let mut events = broker.subscribe();
        let challenge = challenge();
        let responses = tokio::spawn(async move { handler.respond(&challenge).await });
        let event = events.recv().await.expect("challenge event");

        assert!(broker.cancel(&event.auth_id).await);
        assert!(matches!(
            responses.await.expect("join"),
            Err(SshError::Cancelled)
        ));
        assert!(!broker.respond(&event.auth_id, vec!["654321".into()]).await);
    }
}

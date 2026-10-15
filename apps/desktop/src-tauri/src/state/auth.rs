//! One-shot bridge between an SSH keyboard-interactive challenge and the desktop UI.
//!
//! The broker holds no response longer than the single protocol round that requested it.
//! Challenges are ordinary UI events; responses arrive through a command and a oneshot.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{broadcast, oneshot, Mutex};

const CHALLENGE_TIMEOUT: Duration = Duration::from_secs(120);
const EVENT_CAPACITY: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardInteractivePrompt {
    pub prompt: String,
    pub echo: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardInteractiveChallenge {
    pub name: String,
    pub instructions: String,
    pub prompts: Vec<KeyboardInteractivePrompt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthChallengeError {
    Cancelled,
    Authentication(String),
}

impl fmt::Display for AuthChallengeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("SSH authentication challenge was cancelled"),
            Self::Authentication(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for AuthChallengeError {}

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

    /// Cancel every keyboard-interactive round that is currently waiting for
    /// the UI. Used only on host shutdown, where no response can arrive.
    pub async fn cancel_all(&self) -> usize {
        let pending: Vec<_> = self
            .pending
            .lock()
            .await
            .drain()
            .map(|(_, item)| item)
            .collect();
        let mut cancelled = 0;
        for challenge in pending {
            if challenge.sender.send(AuthResponse::Cancelled).is_ok() {
                cancelled += 1;
            }
        }
        cancelled
    }

    pub(crate) async fn request(
        &self,
        server_id: &str,
        username: &str,
        host: &str,
        challenge: &KeyboardInteractiveChallenge,
    ) -> Result<Vec<String>, AuthChallengeError> {
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
            return Err(AuthChallengeError::Authentication(
                "the desktop UI is not listening for SSH authentication challenges".into(),
            ));
        }

        match tokio::time::timeout(CHALLENGE_TIMEOUT, receiver).await {
            Ok(Ok(AuthResponse::Responses(responses))) => Ok(responses),
            Ok(Ok(AuthResponse::Cancelled)) => Err(AuthChallengeError::Cancelled),
            Ok(Err(_)) | Err(_) => {
                self.pending.lock().await.remove(&auth_id);
                Err(AuthChallengeError::Authentication(
                    "the SSH second-factor challenge expired or was closed".into(),
                ))
            }
        }
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
        let request_broker = broker.clone();
        let mut events = broker.subscribe();
        let challenge = challenge();
        let responses = tokio::spawn(async move {
            request_broker
                .request("srv_1", "deploy", "api.example.test", &challenge)
                .await
        });

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
        let request_broker = broker.clone();
        let mut events = broker.subscribe();
        let challenge = challenge();
        let responses = tokio::spawn(async move {
            request_broker
                .request("srv_1", "deploy", "api.example.test", &challenge)
                .await
        });
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
        let request_broker = broker.clone();
        let mut events = broker.subscribe();
        let challenge = challenge();
        let responses = tokio::spawn(async move {
            request_broker
                .request("srv_1", "deploy", "api.example.test", &challenge)
                .await
        });
        let event = events.recv().await.expect("challenge event");

        assert!(broker.cancel(&event.auth_id).await);
        assert!(matches!(
            responses.await.expect("join"),
            Err(AuthChallengeError::Cancelled)
        ));
        assert!(!broker.respond(&event.auth_id, vec!["654321".into()]).await);
    }

    #[tokio::test]
    async fn host_shutdown_cancels_every_waiting_challenge() {
        let broker = AuthChallengeBroker::new();
        let first_broker = broker.clone();
        let second_broker = broker.clone();
        let mut events = broker.subscribe();
        let first = tokio::spawn(async move {
            first_broker
                .request("srv_1", "deploy", "one.test", &challenge())
                .await
        });
        let second = tokio::spawn(async move {
            second_broker
                .request("srv_2", "deploy", "two.test", &challenge())
                .await
        });

        let _ = events.recv().await.expect("first challenge");
        let _ = events.recv().await.expect("second challenge");
        assert_eq!(broker.cancel_all().await, 2);
        assert!(matches!(
            first.await.expect("join"),
            Err(AuthChallengeError::Cancelled)
        ));
        assert!(matches!(
            second.await.expect("join"),
            Err(AuthChallengeError::Cancelled)
        ));
        assert_eq!(broker.cancel_all().await, 0);
    }
}

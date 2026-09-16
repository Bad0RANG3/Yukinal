use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rand::Rng as _;

const PROBE_TTL: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeClaimError {
    Unknown,
    Expired,
    EndpointChanged,
    FingerprintChanged,
}

#[derive(Clone)]
struct PendingProbe {
    ticket: String,
    server_id: String,
    host: String,
    port: u16,
    fingerprint: String,
    expires_at: Instant,
}

/// Server-owned proof that a particular endpoint presented a particular fingerprint.
///
/// The UI receives only an opaque, single-use ticket. It cannot manufacture trust by
/// sending back a fingerprint after editing the server row because `claim` also matches
/// the current row, endpoint, and fingerprint against the pending probe.
#[derive(Default)]
pub struct HostKeyProbeBroker {
    pending: Mutex<HashMap<String, PendingProbe>>,
}

impl HostKeyProbeBroker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn issue(
        &self,
        server_id: &str,
        host: &str,
        port: u16,
        fingerprint: &str,
    ) -> Result<String, String> {
        self.issue_at(server_id, host, port, fingerprint, Instant::now())
    }

    pub fn claim(
        &self,
        ticket: &str,
        server_id: &str,
        host: &str,
        port: u16,
        fingerprint: &str,
    ) -> Result<(), ProbeClaimError> {
        self.claim_at(ticket, server_id, host, port, fingerprint, Instant::now())
    }

    pub fn invalidate(&self, server_id: &str) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.retain(|_, probe| probe.server_id != server_id);
        }
    }

    fn issue_at(
        &self,
        server_id: &str,
        host: &str,
        port: u16,
        fingerprint: &str,
        now: Instant,
    ) -> Result<String, String> {
        let ticket = new_ticket();
        self.pending
            .lock()
            .map_err(|_| "host-key probe broker lock poisoned".to_string())?
            .insert(
                server_id.to_string(),
                PendingProbe {
                    ticket: ticket.clone(),
                    server_id: server_id.to_string(),
                    host: host.to_string(),
                    port,
                    fingerprint: fingerprint.to_string(),
                    expires_at: now + PROBE_TTL,
                },
            );
        Ok(ticket)
    }

    fn claim_at(
        &self,
        ticket: &str,
        server_id: &str,
        host: &str,
        port: u16,
        fingerprint: &str,
        now: Instant,
    ) -> Result<(), ProbeClaimError> {
        let mut pending = self.pending.lock().map_err(|_| ProbeClaimError::Unknown)?;
        let Some(probe) = pending.get(server_id).cloned() else {
            return Err(ProbeClaimError::Unknown);
        };
        if ticket != probe.ticket {
            return Err(ProbeClaimError::Unknown);
        }
        if now >= probe.expires_at {
            pending.remove(server_id);
            return Err(ProbeClaimError::Expired);
        }
        if probe.host != host || probe.port != port {
            pending.remove(server_id);
            return Err(ProbeClaimError::EndpointChanged);
        }
        if probe.fingerprint != fingerprint {
            return Err(ProbeClaimError::FingerprintChanged);
        }
        pending.remove(server_id);
        Ok(())
    }
}

fn new_ticket() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let mut ticket = String::with_capacity(7 + bytes.len() * 2);
    ticket.push_str("probe_");
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(ticket, "{byte:02x}");
    }
    ticket
}

#[cfg(test)]
mod tests {
    use super::*;

    const FINGERPRINT: &str = "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU";

    #[test]
    fn a_probe_ticket_is_single_use() {
        let broker = HostKeyProbeBroker::new();
        let now = Instant::now();
        let ticket = broker
            .issue_at("srv_1", "api.example.com", 22, FINGERPRINT, now)
            .expect("issue");

        assert_eq!(
            broker.claim_at(&ticket, "srv_1", "api.example.com", 22, FINGERPRINT, now,),
            Ok(())
        );
        assert_eq!(
            broker.claim_at(&ticket, "srv_1", "api.example.com", 22, FINGERPRINT, now,),
            Err(ProbeClaimError::Unknown)
        );
    }

    #[test]
    fn an_expired_probe_ticket_is_refused() {
        let broker = HostKeyProbeBroker::new();
        let now = Instant::now();
        let ticket = broker
            .issue_at("srv_1", "api.example.com", 22, FINGERPRINT, now)
            .expect("issue");
        assert_eq!(
            broker.claim_at(
                &ticket,
                "srv_1",
                "api.example.com",
                22,
                FINGERPRINT,
                now + PROBE_TTL,
            ),
            Err(ProbeClaimError::Expired)
        );
    }

    #[test]
    fn changing_the_endpoint_after_probe_invalidates_trust() {
        let broker = HostKeyProbeBroker::new();
        let now = Instant::now();
        let ticket = broker
            .issue_at("srv_1", "api.example.com", 22, FINGERPRINT, now)
            .expect("issue");
        assert_eq!(
            broker.claim_at(
                &ticket,
                "srv_1",
                "attacker.example.com",
                22,
                FINGERPRINT,
                now,
            ),
            Err(ProbeClaimError::EndpointChanged)
        );
    }

    #[test]
    fn changing_the_fingerprint_after_probe_is_refused() {
        let broker = HostKeyProbeBroker::new();
        let now = Instant::now();
        let ticket = broker
            .issue_at("srv_1", "api.example.com", 22, FINGERPRINT, now)
            .expect("issue");
        assert_eq!(
            broker.claim_at(
                &ticket,
                "srv_1",
                "api.example.com",
                22,
                "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                now,
            ),
            Err(ProbeClaimError::FingerprintChanged)
        );
    }

    #[test]
    fn issuing_again_replaces_the_prior_probe() {
        let broker = HostKeyProbeBroker::new();
        let now = Instant::now();
        let first = broker
            .issue_at("srv_1", "api.example.com", 22, FINGERPRINT, now)
            .expect("first");
        let second = broker
            .issue_at("srv_1", "api2.example.com", 22, FINGERPRINT, now)
            .expect("second");

        assert_ne!(first, second);
        assert_eq!(
            broker.claim_at(&first, "srv_1", "api2.example.com", 22, FINGERPRINT, now),
            Err(ProbeClaimError::Unknown)
        );
        assert_eq!(
            broker.claim_at(&second, "srv_1", "api2.example.com", 22, FINGERPRINT, now,),
            Ok(())
        );
    }

    #[test]
    fn invalidation_clears_only_one_server() {
        let broker = HostKeyProbeBroker::new();
        let now = Instant::now();
        let first = broker
            .issue_at("srv_1", "api.example.com", 22, FINGERPRINT, now)
            .expect("first");
        let second = broker
            .issue_at("srv_2", "db.example.com", 22, FINGERPRINT, now)
            .expect("second");

        broker.invalidate("srv_1");
        assert_eq!(
            broker.claim_at(&first, "srv_1", "api.example.com", 22, FINGERPRINT, now),
            Err(ProbeClaimError::Unknown)
        );
        assert_eq!(
            broker.claim_at(&second, "srv_2", "db.example.com", 22, FINGERPRINT, now),
            Ok(())
        );
    }
}

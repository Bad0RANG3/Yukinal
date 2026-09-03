//! `known_hosts` store: strict host-key pinning for non-interactive SSH.
//!
//! Own line format (deliberately not OpenSSH's `~/.ssh/known_hosts` — that format
//! supports hashing/aliases we would have to parse speculatively; our own file is
//! small, versioned and unambiguous):
//!
//! ```text
//! v1:host:port:SHA256:aaaa...
//! ```
//!
//! Rule: a fingerprint the server presents that differs from the pinned one is a
//! MITM indicator and must fail the connection (returns [`Check::Mismatch`]).

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyEntry {
    pub host: String,
    pub port: u16,
    /// `SHA256:base64` (ssh-key fingerprint rendering).
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// Not pinned yet (first-ever connect under the applicable policy).
    Unknown,
    /// Pinned and matches.
    Matches { pinned: String },
    /// Pinned but the server presented something else. Must block.
    Mismatch { pinned: String, presented: String },
}

#[derive(Debug, thiserror::Error)]
pub enum KnownHostsError {
    #[error("failed to read known_hosts at {0}: {1}")]
    Io(String, #[source] std::io::Error),
    #[error("malformed known_hosts line `{0}`")]
    Malformed(String),
}

pub struct KnownHostsStore {
    path: Option<String>,
    entries: HashMap<(String, u16), String>,
    persisted: bool,
}

impl Default for KnownHostsStore {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl KnownHostsStore {
    /// Pinned entries held only in memory (tests / no data dir yet).
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            path: None,
            entries: HashMap::new(),
            persisted: false,
        }
    }

    /// Load from disk, or start empty. A missing file is fine; a malformed file is
    /// an error telling the user where to look.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, KnownHostsError> {
        let path = path.as_ref();
        let mut store = Self::in_memory();
        store.path = Some(path.display().to_string());

        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(store),
            Err(error) => {
                return Err(KnownHostsError::Io(path.display().to_string(), error));
            }
        };

        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let entry = parse_line(line)?;
            store
                .entries
                .insert((entry.host.clone(), entry.port), entry.fingerprint);
        }
        Ok(store)
    }

    #[must_use]
    pub fn check(&self, host: &str, port: u16, presented: &str) -> Check {
        match self.entries.get(&(host.to_string(), port)) {
            None => Check::Unknown,
            Some(pinned) if pinned == presented => Check::Matches {
                pinned: pinned.clone(),
            },
            Some(pinned) => Check::Mismatch {
                pinned: pinned.clone(),
                presented: presented.to_string(),
            },
        }
    }

    /// Pin or replace an entry; persists when a path is configured.
    pub fn register(
        &mut self,
        host: &str,
        port: u16,
        fingerprint: &str,
    ) -> Result<(), KnownHostsError> {
        self.entries
            .insert((host.to_string(), port), fingerprint.to_string());
        self.persisted = true;
        if let Some(path) = &self.path {
            self.save(path)?;
        }
        Ok(())
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), KnownHostsError> {
        let path = path.as_ref();
        let mut out = String::from("# yukinal known_hosts v1 — do not hand-edit lightly\n");
        let mut lines: Vec<HostKeyEntry> = self
            .entries
            .iter()
            .map(|((host, port), fingerprint)| HostKeyEntry {
                host: host.clone(),
                port: *port,
                fingerprint: fingerprint.clone(),
            })
            .collect();
        lines.sort_by(|a, b| (&a.host, a.port).cmp(&(&b.host, b.port)));
        for entry in lines {
            out.push_str(&format!(
                "v1:{}:{}:{}\n",
                entry.host, entry.port, entry.fingerprint
            ));
        }
        std::fs::write(path, out)
            .map_err(|error| KnownHostsError::Io(path.display().to_string(), error))?;
        Ok(())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn parse_line(line: &str) -> Result<HostKeyEntry, KnownHostsError> {
    let mut parts = line.splitn(4, ':');
    let (host, port, fingerprint) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("v1"), Some(host), Some(port), Some(fingerprint)) => (host, port, fingerprint),
        _ => return Err(KnownHostsError::Malformed(line.to_string())),
    };
    let port: u16 = port
        .parse()
        .map_err(|_| KnownHostsError::Malformed(line.to_string()))?;
    if host.is_empty() || !fingerprint.starts_with("SHA256:") || fingerprint.trim().is_empty() {
        return Err(KnownHostsError::Malformed(line.to_string()));
    }
    Ok(HostKeyEntry {
        host: host.to_string(),
        port,
        fingerprint: fingerprint.to_string(),
    })
}

impl fmt::Debug for KnownHostsStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KnownHostsStore")
            .field("path", &self.path)
            .field("entries", &self.entries.len())
            .finish()
    }
}

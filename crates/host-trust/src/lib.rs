//! Local persistence for host-key fingerprints explicitly confirmed in the desktop UI.
//!
//! This crate only stores and compares pins; it has no SSH connection or handshake
//! capability. The C++ SSH service checks the configured pin against the key presented
//! by the remote host.
//!
//! The versioned file uses a narrow, unambiguous format rather than OpenSSH's
//! `known_hosts` syntax, which also supports hashed names and aliases:
//!
//! ```text
//! v1:host:port:SHA256:aaaa...
//! ```
//!
//! A changed fingerprint requires two explicit UI actions: forget the old pin, then
//! probe and confirm the replacement. No single action silently accepts a changed key.

use std::collections::HashMap;
use std::fmt;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

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

/// The three, exhaustive relations between a presented fingerprint and the pin.
///
/// This is a reading of [`Check`], not a fifth concept: [`Check`] also carries *which*
/// fingerprints were involved (the point of ADR 0012 point 3), while this carries only
/// the classification — which is what an IPC `comparison` field and the UI need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    /// This host has never been pinned.
    Unpinned,
    /// The presented fingerprint is the pinned one.
    Matches,
    /// The presented fingerprint is *not* the pinned one. Must block.
    Mismatch,
}

impl Comparison {
    /// The wire word for this classification.
    ///
    /// It lives here rather than being spelled out at each call site because these three
    /// words are part of the contract (`HOST_KEY_MATCH_STATES` in `@yukinal/shared`):
    /// writing `"mismach"` at a call site is not a compile error anywhere.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unpinned => "unpinned",
            Self::Matches => "matches",
            Self::Mismatch => "mismatch",
        }
    }
}

impl Check {
    /// The pin this comparison was made against, if the host is pinned at all.
    #[must_use]
    pub fn pinned(&self) -> Option<&str> {
        match self {
            Self::Unknown => None,
            Self::Matches { pinned } | Self::Mismatch { pinned, .. } => Some(pinned),
        }
    }

    #[must_use]
    pub fn comparison(&self) -> Comparison {
        match self {
            Self::Unknown => Comparison::Unpinned,
            Self::Matches { .. } => Comparison::Matches,
            Self::Mismatch { .. } => Comparison::Mismatch,
        }
    }
}

/// What a `trust` call should do, given the current pin and the fingerprint the user
/// confirmed.
///
/// Pure: no store, no disk, no server. That is the point — the rule that matters here is
/// a policy rule, and policy rules that can only be exercised against a live host are
/// rules nobody exercises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustDecision {
    /// Nothing was pinned: write the confirmed fingerprint.
    Pin { fingerprint: String },
    /// The same fingerprint is already pinned: nothing to do (and nothing to write).
    AlreadyPinned { fingerprint: String },
    /// A **different** fingerprint is pinned: refuse.
    ///
    /// Hard rule from ADR 0012 point 5 — there is deliberately no "accept the new key
    /// anyway" branch. Changing a pin takes two explicit user actions (forget, then
    /// probe-and-confirm), so no *single* action can silently accept a changed key.
    RefusedDifferentPin { pinned: String, confirmed: String },
}

/// Decide what `trust` does. See [`TrustDecision`].
#[must_use]
pub fn decide_trust(pinned: Option<&str>, confirmed: &str) -> TrustDecision {
    match pinned {
        None => TrustDecision::Pin {
            fingerprint: confirmed.to_string(),
        },
        Some(pinned) if pinned == confirmed => TrustDecision::AlreadyPinned {
            fingerprint: confirmed.to_string(),
        },
        Some(pinned) => TrustDecision::RefusedDifferentPin {
            pinned: pinned.to_string(),
            confirmed: confirmed.to_string(),
        },
    }
}

/// What a `forget` call actually did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgetOutcome {
    /// A pin was removed; this is the fingerprint that was there.
    Removed { fingerprint: String },
    /// There was nothing pinned for this `host:port`.
    ///
    /// Not an error (clicking "forget" twice is not a failure) but not silent either:
    /// the caller has to be able to say "本来就没有钉子" instead of claiming a removal
    /// that never happened.
    NothingPinned,
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

    /// The pinned fingerprint for `host:port`, if this host has been pinned.
    ///
    /// This is the lookup the connection path uses. It exists so that callers which
    /// only want the pin do not have to reach for [`Self::check`] and invent a
    /// `presented` value: `establish` used to call `check(host, port, "")` and
    /// collapse `Matches` and `Mismatch` together, which read as "compare against
    /// nothing" and hid the fact that `Check::Mismatch` never fires in production.
    /// A lookup should not be spelled as a comparison with a sentinel.
    #[must_use]
    pub fn pinned_fingerprint(&self, host: &str, port: u16) -> Option<String> {
        self.entries.get(&(host.to_string(), port)).cloned()
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
    ///
    /// Callers that pin on a **user's** behalf should use [`Self::trust`] instead: this
    /// method replaces whatever was there, which is exactly the "silently accept a
    /// changed key" behaviour ADR 0012 point 5 forbids. It stays for the TOFU path,
    /// where `establish` has already established that nothing was pinned.
    pub fn register(
        &mut self,
        host: &str,
        port: u16,
        fingerprint: &str,
    ) -> Result<(), KnownHostsError> {
        let key = (host.to_string(), port);
        let previous = self.entries.insert(key.clone(), fingerprint.to_string());
        if let Some(path) = &self.path {
            if let Err(error) = self.save(path) {
                match previous {
                    Some(previous) => {
                        self.entries.insert(key, previous);
                    }
                    None => {
                        self.entries.remove(&key);
                    }
                }
                return Err(error);
            }
        }
        Ok(())
    }

    /// Pin the fingerprint the user **confirmed**.
    ///
    /// The decision is the pure function [`decide_trust`]; this method only applies it
    /// (and persists). It therefore cannot drift from the tested rule: a differing
    /// fingerprint comes back as [`TrustDecision::RefusedDifferentPin`] and **nothing is
    /// written**.
    ///
    /// Note the refusal is not an error: it is a policy answer the caller has to render
    /// ("先遗忘旧的钉子"), which is why it is returned rather than reported as an IO-style
    /// failure of the store.
    pub fn trust(
        &mut self,
        host: &str,
        port: u16,
        confirmed: &str,
    ) -> Result<TrustDecision, KnownHostsError> {
        let decision = decide_trust(self.pinned_fingerprint(host, port).as_deref(), confirmed);
        if matches!(decision, TrustDecision::Pin { .. }) {
            self.register(host, port, confirmed)?;
        }
        Ok(decision)
    }

    /// Remove the pin for `host:port` and persist the removal.
    ///
    /// Returns what actually happened ([`ForgetOutcome`]) instead of `()`: "there was
    /// nothing to forget" is a different answer from "removed SHA256:…", and a caller
    /// that cannot tell them apart ends up claiming a removal that never happened.
    ///
    /// A failed save rolls the in-memory removal back. Without that, the running process
    /// would believe the pin is gone (the user reads "已遗忘") while the file still has
    /// it — and the pin would be back on the next launch, silently.
    pub fn forget(&mut self, host: &str, port: u16) -> Result<ForgetOutcome, KnownHostsError> {
        let key = (host.to_string(), port);
        let Some(fingerprint) = self.entries.remove(&key) else {
            return Ok(ForgetOutcome::NothingPinned);
        };
        if let Some(path) = &self.path {
            if let Err(error) = self.save(path) {
                self.entries.insert(key, fingerprint);
                return Err(error);
            }
        }
        Ok(ForgetOutcome::Removed { fingerprint })
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
        write_atomic(path, out.as_bytes())
            .map_err(|error| KnownHostsError::Io(path.display().to_string(), error))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;

    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(contents)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    persist_with_retry(temporary, path)?;

    // Persist is atomic for the file itself. Syncing the directory makes the rename
    // durable across a crash on platforms that expose directory fsync.
    #[cfg(unix)]
    if let Err(error) = std::fs::File::open(parent).and_then(|directory| directory.sync_all()) {
        // The rename already committed. Reporting this as a failed save would make the
        // caller roll its in-memory pin back while the file contains the new value.
        eprintln!(
            "known_hosts was replaced, but syncing directory {} failed: {error}",
            parent.display()
        );
    }

    Ok(())
}

fn persist_with_retry(mut temporary: tempfile::NamedTempFile, path: &Path) -> std::io::Result<()> {
    let mut delay = Duration::from_millis(1);
    for attempt in 0..8 {
        match temporary.persist(path) {
            Ok(_) => return Ok(()),
            Err(error)
                if error.error.kind() == std::io::ErrorKind::PermissionDenied && attempt < 7 =>
            {
                // Windows can return ERROR_ACCESS_DENIED while another process is
                // replacing the same destination. Retrying the rename preserves the
                // atomic protocol instead of falling back to an in-place truncation.
                temporary = error.file;
                std::thread::sleep(delay);
                delay *= 2;
            }
            Err(error) => return Err(error.error),
        }
    }
    unreachable!("the loop either returns or retries while attempt < 7")
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

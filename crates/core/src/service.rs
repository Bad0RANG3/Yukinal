//! Safe systemd service references and bounded command construction.
//!
//! The desktop command layer owns the SSH session and I/O.  This module keeps
//! the model-controlled part of a service action small and testable: only
//! `.service` unit names are accepted, and every remote command is built from
//! a shell-quoted value.

use serde::Serialize;

use crate::docker::shell_quote;

/// Default timeout for a systemd restart when the Agent omits one.
pub const DEFAULT_SYSTEMD_RESTART_TIMEOUT: usize = 30;
/// Upper bound for a systemd restart request.
pub const MAX_SYSTEMD_RESTART_TIMEOUT: usize = 120;
/// Maximum output accepted by the normalized `systemd.inspect` parser.
pub const MAX_SYSTEMD_INSPECT_CHARS: usize = 8_192;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SystemdInspectResult {
    pub service: String,
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SystemdRestartResult {
    pub service: String,
    pub restarted: bool,
}

/// A deliberately conservative systemd unit reference.
///
/// The service name is inserted into a remote shell command.  Requiring the
/// conventional `.service` suffix prevents a caller from selecting a mount,
/// socket, path or target unit, while the character whitelist prevents option
/// injection and shell syntax from reaching the SSH backend.
#[must_use]
pub fn is_safe_systemd_service_ref(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    value.len() <= 128
        && value.ends_with(".service")
        && first.is_ascii_alphanumeric()
        && chars.all(|character| character.is_ascii_alphanumeric() || "_.@:-".contains(character))
}

/// Build a bounded, non-interactive `systemctl show` command.
#[must_use]
pub fn systemd_inspect_command(service: &str) -> String {
    format!(
        "systemctl show --no-pager --property=LoadState,ActiveState,SubState,Description --value -- {}",
        shell_quote(service)
    )
}

/// Build a non-interactive restart command.  The caller supplies the timeout
/// to the SSH execution budget; it is not interpolated into the command.
#[must_use]
pub fn systemd_restart_command(service: &str) -> String {
    format!(
        "systemctl restart --no-ask-password --wait -- {}",
        shell_quote(service)
    )
}

/// Parse the four value lines emitted by `systemctl show --value`.
pub fn parse_systemd_inspect(raw: &str, service: &str) -> Result<SystemdInspectResult, String> {
    if !is_safe_systemd_service_ref(service) {
        return Err("service must be a safe .service unit reference".into());
    }
    if raw.chars().count() > MAX_SYSTEMD_INSPECT_CHARS {
        return Err("systemd inspect output exceeded its size limit".into());
    }
    let mut values = raw.lines();
    let load_state = required_line(values.next(), "LoadState")?;
    let active_state = required_line(values.next(), "ActiveState")?;
    let sub_state = required_line(values.next(), "SubState")?;
    let description = values
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if values.next().is_some() {
        return Err("systemd inspect returned unexpected extra fields".into());
    }
    Ok(SystemdInspectResult {
        service: service.to_string(),
        load_state,
        active_state,
        sub_state,
        description,
    })
}

fn required_line(line: Option<&str>, field: &str) -> Result<String, String> {
    line.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("systemd inspect omitted {field}"))
}

#[cfg(test)]
mod tests {
    use super::{
        is_safe_systemd_service_ref, parse_systemd_inspect, systemd_inspect_command,
        systemd_restart_command,
    };

    #[test]
    fn service_references_are_conservative_and_shell_safe() {
        assert!(is_safe_systemd_service_ref("nginx.service"));
        assert!(is_safe_systemd_service_ref("yukinal-api@blue.service"));
        assert!(!is_safe_systemd_service_ref("nginx"));
        assert!(!is_safe_systemd_service_ref("-nginx.service"));
        assert!(!is_safe_systemd_service_ref("nginx.service;rm -rf /"));
        assert!(!is_safe_systemd_service_ref("docker.socket"));
    }

    #[test]
    fn commands_quote_the_unit_and_do_not_accept_shell_fragments() {
        assert_eq!(
            systemd_inspect_command("nginx.service"),
            "systemctl show --no-pager --property=LoadState,ActiveState,SubState,Description --value -- 'nginx.service'"
        );
        assert_eq!(
            systemd_restart_command("nginx.service"),
            "systemctl restart --no-ask-password --wait -- 'nginx.service'"
        );
    }

    #[test]
    fn inspect_normalizes_the_bounded_value_shape() {
        let result = parse_systemd_inspect(
            "loaded\nactive\nrunning\nNginx web server\n",
            "nginx.service",
        )
        .expect("inspect output");
        assert_eq!(result.load_state, "loaded");
        assert_eq!(result.active_state, "active");
        assert_eq!(result.sub_state, "running");
        assert_eq!(result.description.as_deref(), Some("Nginx web server"));
    }

    #[test]
    fn inspect_rejects_missing_or_extra_fields() {
        assert!(parse_systemd_inspect("loaded\nactive\n", "nginx.service").is_err());
        assert!(
            parse_systemd_inspect("loaded\nactive\nrunning\ndesc\nextra\n", "nginx.service")
                .is_err()
        );
    }
}

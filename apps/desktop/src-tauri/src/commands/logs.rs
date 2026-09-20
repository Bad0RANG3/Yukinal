//! Bounded, read-only remote log access.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::State;
use tokio_util::sync::CancellationToken;
use yukinal_ssh::SshBackend;

use crate::commands::terminal::ensure_session;
use crate::state::AppState;

const LOG_DISCOVERY_COMMAND: &str = r#"if command -v journalctl >/dev/null 2>&1; then journalctl -n 120 --no-pager -o short-iso 2>/dev/null; if [ $? -eq 0 ]; then printf '\n__YUKINAL_SOURCE__=journalctl\n'; exit 0; fi; fi; for file in /var/log/syslog /var/log/messages; do if [ -r "$file" ]; then tail -n 120 "$file"; if [ $? -eq 0 ]; then case "$file" in /var/log/syslog) printf '\n__YUKINAL_SOURCE__=syslog\n' ;; /var/log/messages) printf '\n__YUKINAL_SOURCE__=messages\n' ;; esac; exit 0; fi; fi; done; printf '__YUKINAL_SOURCE__=unavailable\n'"#;
const SOURCE_PREFIX: &str = "__YUKINAL_SOURCE__=";
const MAX_LOG_LINES: usize = 120;
const MAX_LOG_SINCE_SECONDS: u32 = 86_400;

pub(crate) fn log_discovery_command() -> &'static str {
    LOG_DISCOVERY_COMMAND
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ServerLogsInput {
    pub since_seconds: Option<u32>,
    pub unit: Option<String>,
}

/// Build the fixed read-only probe with optional numeric/unit filters. The values are
/// validated before interpolation; no model-provided shell fragment reaches the command.
pub(crate) fn log_discovery_command_for(input: &ServerLogsInput) -> Result<String, String> {
    if input.since_seconds.is_none() && input.unit.is_none() {
        return Ok(LOG_DISCOVERY_COMMAND.to_string());
    }
    if input
        .since_seconds
        .is_some_and(|seconds| !(1..=MAX_LOG_SINCE_SECONDS).contains(&seconds))
    {
        return Err(format!(
            "server.logs sinceSeconds must be between 1 and {MAX_LOG_SINCE_SECONDS}"
        ));
    }
    if let Some(unit) = input.unit.as_deref() {
        if !is_safe_systemd_unit(unit) {
            return Err("server.logs unit must be a bounded .service reference".into());
        }
    }
    let since = input
        .since_seconds
        .map(|seconds| format!(" --since '-{seconds} seconds'"))
        .unwrap_or_default();
    let unit = input
        .unit
        .as_deref()
        .map(|value| format!(" --unit '{}'", shell_single_quote(value)))
        .unwrap_or_default();
    Ok(format!(
        "if command -v journalctl >/dev/null 2>&1; then journalctl -n 120 --no-pager -o short-iso{since}{unit} 2>/dev/null; if [ $? -eq 0 ]; then printf '\\n__YUKINAL_SOURCE__=journalctl\\n'; exit 0; fi; fi; for file in /var/log/syslog /var/log/messages; do if [ -r \"$file\" ]; then tail -n 120 \"$file\"; if [ $? -eq 0 ]; then case \"$file\" in /var/log/syslog) printf '\\n__YUKINAL_SOURCE__=syslog\\n' ;; /var/log/messages) printf '\\n__YUKINAL_SOURCE__=messages\\n' ;; esac; exit 0; fi; fi; done; printf '__YUKINAL_SOURCE__=unavailable\\n'"
    ))
}

fn is_safe_systemd_unit(value: &str) -> bool {
    let length = value.chars().count();
    if !(1..=128).contains(&length) || !value.ends_with(".service") {
        return false;
    }
    let mut chars = value.chars();
    if !chars
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric())
    {
        return false;
    }
    chars.all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '@' | ':' | '-')
    })
}

fn shell_single_quote(value: &str) -> String {
    value.replace('\'', "'\\''")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogSource {
    Journalctl,
    Syslog,
    Messages,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerLogLine {
    pub text: String,
    pub level: LogLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerLogsResponse {
    pub source: LogSource,
    pub lines: Vec<ServerLogLine>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `server_logs`: connect if necessary, then read only the most recent 120 lines.
#[tauri::command]
pub async fn server_logs(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<ServerLogsResponse, String> {
    ensure_session(&state, &server_id).await?;
    let session = state
        .terminals
        .cached_session(&server_id)
        .map_err(|error| error.to_string())?;
    let output = state
        .ssh
        .execute(
            &session,
            log_discovery_command(),
            Some(Duration::from_secs(10)),
            &CancellationToken::new(),
        )
        .await
        .map_err(|error| error.to_string())?;

    parse_logs_output(&output.stdout_lossy())
        .map_err(|error| format!("log discovery returned an invalid response: {error}"))
}

pub(crate) fn parse_logs_output(raw: &str) -> Result<ServerLogsResponse, String> {
    let source = raw
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(SOURCE_PREFIX))
        .map(parse_source)
        .transpose()?
        .ok_or_else(|| "missing log source marker".to_string())?;

    let lines = match source {
        LogSource::Unavailable => Vec::new(),
        LogSource::Journalctl | LogSource::Syslog | LogSource::Messages => raw
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.trim().is_empty() && !line.trim().starts_with(SOURCE_PREFIX))
            .map(|line| ServerLogLine {
                text: line.to_string(),
                level: classify_level(line),
            })
            .take(MAX_LOG_LINES)
            .collect(),
    };
    let message = (source == LogSource::Unavailable)
        .then(|| "未检测到可读取的 journalctl 或系统日志文件".to_string());
    Ok(ServerLogsResponse {
        source,
        lines,
        message,
    })
}

fn parse_source(source: &str) -> Result<LogSource, String> {
    match source {
        "journalctl" => Ok(LogSource::Journalctl),
        "syslog" => Ok(LogSource::Syslog),
        "messages" => Ok(LogSource::Messages),
        "unavailable" => Ok(LogSource::Unavailable),
        other => Err(format!("unknown log source `{other}`")),
    }
}

fn classify_level(line: &str) -> LogLevel {
    let lower = line.to_ascii_lowercase();
    if [
        "panic", "emerg", "alert", "crit", "error", "failed", "failure",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        LogLevel::Error
    } else if ["warning", "warn", "degraded"]
        .iter()
        .any(|marker| lower.contains(marker))
    {
        LogLevel::Warning
    } else {
        LogLevel::Info
    }
}

#[cfg(test)]
mod tests {
    use super::{
        log_discovery_command_for, parse_logs_output, LogLevel, LogSource, ServerLogLine,
        ServerLogsInput, ServerLogsResponse,
    };

    const FIXTURE: &str =
        include_str!("../../../../../packages/shared/fixtures/ipc/server_logs.json");

    #[test]
    fn parses_bounded_journal_lines_and_classifies_severity() {
        let response = parse_logs_output(concat!(
            "2026-09-04T06:00:00+0800 host sshd[10]: Accepted publickey\n",
            "2026-09-04T06:01:00+0800 host nginx[12]: warning: worker restarted\n",
            "2026-09-04T06:02:00+0800 host app[13]: Failed to connect to database\n",
            "__YUKINAL_SOURCE__=journalctl\n",
        ))
        .expect("journal output");

        assert_eq!(response.source, LogSource::Journalctl);
        assert_eq!(response.lines.len(), 3);
        assert_eq!(response.lines[0].level, LogLevel::Info);
        assert_eq!(response.lines[1].level, LogLevel::Warning);
        assert_eq!(response.lines[2].level, LogLevel::Error);
        assert!(response.lines[2].text.contains("Failed to connect"));
    }

    #[test]
    fn unavailable_source_is_explicit_and_has_no_fake_lines() {
        let response = parse_logs_output("__YUKINAL_SOURCE__=unavailable\n").expect("marker");

        assert_eq!(response.source, LogSource::Unavailable);
        assert!(response.lines.is_empty());
        assert!(response.message.is_some());
    }

    #[test]
    fn filtered_log_command_is_bounded_and_does_not_accept_shell_fragments() {
        let command = log_discovery_command_for(&ServerLogsInput {
            since_seconds: Some(3_600),
            unit: Some("nginx.service".into()),
        })
        .expect("filtered command");
        assert!(command.contains("--since '-3600 seconds'"));
        assert!(command.contains("--unit 'nginx.service'"));
        assert!(log_discovery_command_for(&ServerLogsInput {
            since_seconds: Some(86_401),
            unit: None,
        })
        .is_err());
        assert!(log_discovery_command_for(&ServerLogsInput {
            since_seconds: None,
            unit: Some("nginx.service;id".into()),
        })
        .is_err());
    }

    #[test]
    fn serializes_to_the_shared_contract_fixture() {
        let actual = serde_json::to_value(ServerLogsResponse {
            source: LogSource::Journalctl,
            lines: vec![
                ServerLogLine {
                    text: "2026-09-04T06:00:00+0800 host sshd[10]: Accepted publickey".into(),
                    level: LogLevel::Info,
                },
                ServerLogLine {
                    text: "2026-09-04T06:02:00+0800 host app[13]: Failed to connect to database"
                        .into(),
                    level: LogLevel::Error,
                },
            ],
            message: None,
        })
        .expect("serialize");
        let expected: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture");
        assert_eq!(actual, expected);
    }
}

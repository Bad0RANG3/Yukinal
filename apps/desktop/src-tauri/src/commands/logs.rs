//! Bounded, read-only remote log access.

use std::time::Duration;

use serde::Serialize;
use tauri::State;
use tokio_util::sync::CancellationToken;
use yukinal_ssh::SshBackend;

use crate::commands::terminal::ensure_session;
use crate::state::AppState;

const LOG_DISCOVERY_COMMAND: &str = r#"if command -v journalctl >/dev/null 2>&1; then journalctl -n 120 --no-pager -o short-iso 2>/dev/null; if [ $? -eq 0 ]; then printf '\n__YUKINAL_SOURCE__=journalctl\n'; exit 0; fi; fi; for file in /var/log/syslog /var/log/messages; do if [ -r "$file" ]; then tail -n 120 "$file"; if [ $? -eq 0 ]; then case "$file" in /var/log/syslog) printf '\n__YUKINAL_SOURCE__=syslog\n' ;; /var/log/messages) printf '\n__YUKINAL_SOURCE__=messages\n' ;; esac; exit 0; fi; fi; done; printf '__YUKINAL_SOURCE__=unavailable\n'"#;
const SOURCE_PREFIX: &str = "__YUKINAL_SOURCE__=";
const MAX_LOG_LINES: usize = 120;

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
            LOG_DISCOVERY_COMMAND,
            Some(Duration::from_secs(10)),
            &CancellationToken::new(),
        )
        .await
        .map_err(|error| error.to_string())?;

    parse_logs_output(&output.stdout_lossy())
        .map_err(|error| format!("log discovery returned an invalid response: {error}"))
}

fn parse_logs_output(raw: &str) -> Result<ServerLogsResponse, String> {
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
    use super::{parse_logs_output, LogLevel, LogSource, ServerLogLine, ServerLogsResponse};

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

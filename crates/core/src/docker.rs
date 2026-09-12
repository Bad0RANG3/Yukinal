//! Docker 工具族的纯规则：`docker ps` / `docker inspect` 行的解析、命令行构造，以及日志输出的
//! 有界化。
//!
//! 按本仓库的分层规则（`apps/desktop/src-tauri` 只做参数编组与事件转发，真逻辑放 `crates/*`），
//! 这些规则原先住在 `apps/desktop/src-tauri/src/commands/host.rs` 里。它们不碰 SSH、不碰
//! `AppState`、不碰取消令牌 —— 输入是一段文本、输出是一个值或一条命令，所以能在本 crate 里
//! 直接测试。
//!
//! 留在命令层的是**要做 I/O 的那一半**：打开会话、跑命令、把失败映射成 host 协议的失败码。
//!
//! 两条规则值得单独点名，因为它们是安全边界而不是格式细节：
//! - [`is_safe_container_ref`] + [`shell_quote`]：容器名来自模型，会被拼进 shell 命令行。
//! - [`bounded_log_lines`] / [`truncate_text`]：远端输出没有上限，而工具结果会进模型上下文。

use serde::{Deserialize, Serialize};
use yukinal_database::models::ContainerInfo;

/// 一次 `docker.ps` 最多返回多少个容器。
pub const MAX_CONTAINERS: usize = 200;
/// `docker.logs` 没给 `tail` 时的默认行数。
pub const DEFAULT_LOG_TAIL: usize = 120;
/// `tail` 的上限：这是「工具结果进模型上下文」的闸门。
pub const MAX_LOG_TAIL: usize = 500;
/// 单行日志的字符上限（超过就截断并标 `truncated`）。
pub const MAX_LOG_LINE_CHARS: usize = 4_000;
/// `docker.restart` 没给 `timeoutSeconds` 时的默认值。
pub const DEFAULT_RESTART_TIMEOUT: usize = 10;
/// `timeoutSeconds` 的上限。
pub const MAX_RESTART_TIMEOUT: usize = 120;
pub const DOCKER_PS_COMMAND: &str = "docker ps --format '{{json .}}' 2>/dev/null";
pub const DOCKER_PS_ALL_COMMAND: &str = "docker ps -a --format '{{json .}}' 2>/dev/null";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerRow {
    names: Option<String>,
    image: Option<String>,
    state: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerInspectRow {
    id: Option<String>,
    name: Option<String>,
    config: Option<DockerConfig>,
    state: Option<DockerState>,
    restart_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerConfig {
    image: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerState {
    status: Option<String>,
    started_at: Option<String>,
    health: Option<DockerHealth>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerHealth {
    status: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerLogsResult {
    pub container: String,
    pub lines: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerInspectResult {
    id: String,
    name: String,
    image: String,
    state: String,
    status: String,
    restart_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    health: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerRestartResult {
    pub container: String,
    pub restarted: bool,
}

/// `docker restart` 的命令行。`--time` 有上限，容器名一定过 [`shell_quote`]。
#[must_use]
pub fn docker_restart_command(container: &str, timeout: usize) -> String {
    format!(
        "docker restart --time {timeout} -- {}",
        shell_quote(container)
    )
}

/// `docker inspect --format json` 的输出 → 归一化后的行。
///
/// 只取我们声明过的字段（`id` / `name` / `image` / `state` / `restartCount` / `startedAt` /
/// `health`），**不把 docker 的原始形状转发出去**：那是一份会随 docker 版本变动的结构，转发
/// 等于把「工具的返回形状」交给别人的发布节奏决定。
pub fn parse_docker_inspect(raw: &str) -> Result<DockerInspectResult, String> {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| "docker inspect returned no JSON object".to_string())?;
    let row = serde_json::from_str::<DockerInspectRow>(line)
        .map_err(|error| format!("docker inspect returned invalid JSON: {error}"))?;
    let id = nonempty(row.id, "Id")?;
    let name = row
        .name
        .map(|value| value.trim_start_matches('/').to_string())
        .filter(|value| is_safe_container_ref(value))
        .ok_or_else(|| "docker inspect returned an invalid Name".to_string())?;
    let image = nonempty(row.config.and_then(|config| config.image), "Config.Image")?;
    let state = row
        .state
        .ok_or_else(|| "docker inspect omitted State".to_string())?;
    let status = nonempty(state.status, "State.Status")?;
    let started_at = state.started_at.filter(|value| !value.trim().is_empty());
    let health = state
        .health
        .and_then(|health| health.status)
        .filter(|value| !value.trim().is_empty());
    Ok(DockerInspectResult {
        id,
        name,
        image,
        state: status.clone(),
        status,
        restart_count: row.restart_count.unwrap_or(0),
        started_at,
        health,
    })
}

fn nonempty(value: Option<String>, field: &str) -> Result<String, String> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string())
        .ok_or_else(|| format!("docker inspect omitted {field}"))
}

/// 容器引用（名字或 id）的形状。
///
/// 这个名字来自模型，会被拼进远端 shell 命令行，所以是**白名单**：首字符必须是 ASCII
/// 字母数字，其余只能是字母数字与 `_.-`，并且有长度上限。`api;rm -rf /` 这种输入在这里
/// 就被挡掉，不会走到 [`shell_quote`]。
#[must_use]
pub fn is_safe_container_ref(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    value.len() <= 128
        && first.is_ascii_alphanumeric()
        && chars.all(|character| character.is_ascii_alphanumeric() || "_.-".contains(character))
}

/// POSIX 单引号转义：`'` → `'\''`。
#[must_use]
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// 日志行的有界化：最多 `limit` 行，每行最多 [`MAX_LOG_LINE_CHARS`] 字符。
///
/// 两处越界都会把 `truncated` 置真 —— 「行数被砍」和「某一行被砍」对读结果的人是两件
/// 不同的事，但都对「这份日志完整吗」这个问题回答「不」。
pub fn bounded_log_lines(raw: &str, limit: usize) -> (Vec<String>, bool) {
    let mut truncated = false;
    let lines = raw
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            if index >= limit {
                truncated = true;
                return None;
            }
            let bounded = truncate_text(line, MAX_LOG_LINE_CHARS);
            if bounded.chars().count() < line.chars().count() {
                truncated = true;
            }
            Some(bounded)
        })
        .collect();
    (lines, truncated)
}

pub fn truncate_text(value: &str, max_chars: usize) -> String {
    let mut output: String = value.chars().take(max_chars).collect();
    if value.chars().count() > max_chars {
        output.push('…');
    }
    output
}

pub fn parse_docker_ps(raw: &str) -> Vec<ContainerInfo> {
    raw.lines()
        .filter_map(|line| serde_json::from_str::<DockerRow>(line.trim()).ok())
        .filter_map(|row| {
            let name = row.names?.split(',').next()?.trim().to_string();
            let image = row.image?.trim().to_string();
            let state = row.state?.trim().to_string();
            let status = row.status?.trim().to_string();
            if name.is_empty() || image.is_empty() || state.is_empty() || status.is_empty() {
                return None;
            }
            Some(ContainerInfo {
                name,
                image,
                state,
                status,
                // `docker ps` is intentionally bounded to listing; restartCount
                // belongs to the later inspect tool and is not guessed here.
                restart_count: 0,
            })
        })
        .take(MAX_CONTAINERS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        bounded_log_lines, docker_restart_command, is_safe_container_ref, parse_docker_inspect,
        parse_docker_ps, shell_quote,
    };

    #[test]
    fn parses_docker_json_lines_into_bounded_structured_rows() {
        let rows = parse_docker_ps(
            r#"{"Names":"web,web-old","Image":"nginx:1.27","State":"running","Status":"Up 3 hours"}
{"Names":"db","Image":"postgres:16","State":"exited","Status":"Exited (0) 2 days ago"}
not-json
"#,
        );

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "web");
        assert_eq!(rows[0].restart_count, 0);
        assert_eq!(rows[1].state, "exited");
    }

    #[test]
    fn drops_rows_missing_required_fields() {
        let rows = parse_docker_ps(
            r#"{"Names":"","Image":"nginx","State":"running","Status":"Up"}
{"Names":"ok","Image":"","State":"running","Status":"Up"}
"#,
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn bounds_log_lines_and_marks_long_output() {
        let long_line = "x".repeat(4_010);
        let raw = format!("first\n{long_line}\nthird\n");
        let (lines, truncated) = bounded_log_lines(&raw, 2);

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "first");
        assert!(lines[1].ends_with('…'));
        assert!(truncated);
    }

    #[test]
    fn parses_normalized_inspect_fields_without_forwarding_raw_docker_shape() {
        let result = parse_docker_inspect(
            r#"{"Id":"sha256:abc","Name":"/web","Config":{"Image":"nginx:1.27"},"State":{"Status":"running","StartedAt":"2026-09-04T06:00:00Z","Health":{"Status":"healthy"}},"RestartCount":2}"#,
        )
        .expect("inspect output");

        assert_eq!(result.id, "sha256:abc");
        assert_eq!(result.name, "web");
        assert_eq!(result.state, "running");
        assert_eq!(result.restart_count, 2);
        assert_eq!(result.health.as_deref(), Some("healthy"));
    }

    #[test]
    fn container_reference_validation_and_shell_quote_are_defensive() {
        assert!(is_safe_container_ref("api_1.2-3"));
        assert!(!is_safe_container_ref("api;rm -rf /"));
        assert_eq!(shell_quote("api_1"), "'api_1'");
    }

    #[test]
    fn restart_command_is_bounded_and_shell_safe() {
        assert_eq!(
            docker_restart_command("api_1", 15),
            "docker restart --time 15 -- 'api_1'"
        );
    }
}

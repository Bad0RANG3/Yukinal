//! 远端路径策略：文件能力唯一的规则来源。
//!
//! 这些规则原先住在 `apps/desktop/src-tauri/src/commands/host.rs`。搬进来之后判据只有一条：
//! 改规则 = 改这里。上层不再各持一份，也不会出现「宿主工具拦了、别的入口忘了拦」。

use crate::limits::MAX_REMOTE_PATH_CHARS;

/// 命中凭据/进程密钥黑名单时给 Agent 的拒绝文案。
///
/// 文案本身就是对外契约：它原样进入工具结果的 `denied_by_policy.message`。把它和规则放在
/// 一起，是为了避免出现「规则改了、这句话没跟上」这种只有用户看得见的偏差。
pub const AGENT_PATH_POLICY_MESSAGE: &str =
    "Agent file tools cannot access paths that commonly contain credentials or process secrets";

/// 规则一：路径里**出现**这些片段即拒绝（目录型目标：凭据目录与进程信息目录）。
///
/// 匹配的是片段而不是前缀，因为目标路径通常带前缀（`/home/deploy/.ssh/id_rsa`）。两端都带
/// 斜杠的写法让 `/.ssh/` 不会误伤 `/.sshfs-cache/` 这类同前缀目录。
const BLOCKED_PATH_SUBSTRINGS: &[&str] = &[
    "/.aws/",
    "/.azure/",
    "/.config/gcloud/",
    "/.kube/",
    "/.ssh/",
    "/proc/",
    "/run/secrets/",
    "/var/run/secrets/",
];

/// 规则二：文件名整体命中即拒绝（系统凭据文件，以及常见的凭据清单文件名）。
const BLOCKED_FILE_NAMES: &[&str] = &[
    "shadow",
    "gshadow",
    "sudoers",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "credentials",
    "credentials.json",
    "secrets",
    "secrets.json",
];

/// 规则三：后缀命中即拒绝（私钥与密钥库容器）。
const BLOCKED_FILE_EXTENSIONS: &[&str] = &[".pem", ".key", ".p12", ".pfx", ".jks"];

/// `.env` 家族的例外：模板文件按惯例只放键名与占位值，真实密钥不在这里。
///
/// 例外按**归一化后**的文件名比较，所以 `.ENV.EXAMPLE` 同样放行 —— 大小写不敏感是整条策略
/// 的性质（远端文件名可能来自别的系统），不是这一条的特例。
const ENV_TEMPLATE_NAMES: &[&str] = &[".env.example", ".env.sample", ".env.template"];

/// 远端路径的形状校验：绝对路径、非空、长度受限、不含控制字符。
///
/// 返回 `Err(文案)` 而不是一个错误枚举：这句话会原样出现在工具结果的
/// `invalid_input.message` 里，**文案就是这里的接口**，分类留给上层。
///
/// 长度按**字符**而不是字节数：这个上限是给「人类可读的路径」设的，一个中文文件名不该因为
/// UTF-8 编码而占掉三倍配额。
pub fn validate_remote_path(value: &str) -> Result<(), String> {
    if value.is_empty() || value.chars().count() > MAX_REMOTE_PATH_CHARS {
        return Err(format!(
            "remote path must be 1-{MAX_REMOTE_PATH_CHARS} characters"
        ));
    }
    if !value.starts_with('/') {
        return Err("remote path must be absolute".to_string());
    }
    if value
        .bytes()
        .any(|byte| byte == 0 || byte == b'\r' || byte == b'\n')
    {
        return Err("remote path contains a forbidden control character".to_string());
    }
    Ok(())
}

/// Agent 文件工具必须不能变成读凭据或覆盖凭据的原语。
///
/// 判定在**宿主进程**（Tauri 侧）执行：即使 sidecar 被攻破、伪造了入参，这一关也拦在真正
/// 打开 SFTP 之前（`RemoteFileService` 在构造 `Agent*Request` 时就查，见服务模块）。
#[must_use]
pub fn is_agent_blocked_path(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase();
    if BLOCKED_PATH_SUBSTRINGS
        .iter()
        .any(|needle| normalized.contains(needle))
    {
        return true;
    }

    // 末段即文件名；末尾带斜杠的目录目标会得到空名字，那类目标靠规则一拦。
    let name = normalized.rsplit('/').next().unwrap_or_default();
    if BLOCKED_FILE_NAMES.contains(&name)
        || BLOCKED_FILE_EXTENSIONS
            .iter()
            .any(|extension| name.ends_with(extension))
    {
        return true;
    }
    if name == ".env" || name.starts_with(".env.") {
        return !ENV_TEMPLATE_NAMES.contains(&name);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{is_agent_blocked_path, validate_remote_path};
    use crate::limits::MAX_REMOTE_PATH_CHARS;

    #[test]
    fn remote_file_paths_are_absolute_and_bounded() {
        assert!(validate_remote_path("/etc/app.env").is_ok());
        assert!(validate_remote_path("relative/app.env").is_err());
        assert!(validate_remote_path("/etc/app\n.env").is_err());
        assert!(validate_remote_path(&format!("/{}", "x".repeat(4_096))).is_err());
    }

    #[test]
    fn paths_must_be_non_empty_and_free_of_control_characters() {
        assert!(validate_remote_path("").is_err());
        // NUL / CR / LF 会截断或拆散 SFTP 请求里的路径，一律拒绝。
        assert!(validate_remote_path("/etc/app\0.env").is_err());
        assert!(validate_remote_path("/etc/app\r.env").is_err());
        // 制表符不在禁止之列：它不截断路径，只是文件名里少见。
        assert!(validate_remote_path("/etc/app\t.env").is_ok());
    }

    #[test]
    fn the_path_length_limit_counts_characters_not_bytes() {
        // 4095 个汉字 = 12285 字节，仍然合法：上限是给人类可读路径设的字符数。
        let wide = format!("/{}", "文".repeat(MAX_REMOTE_PATH_CHARS - 1));
        assert_eq!(wide.chars().count(), MAX_REMOTE_PATH_CHARS);
        assert!(wide.len() > MAX_REMOTE_PATH_CHARS);
        assert!(validate_remote_path(&wide).is_ok());

        // 恰好在上限上合法，多一个字符即拒绝 —— 边界两边的行为都要钉住。
        let at_limit = format!("/{}", "x".repeat(MAX_REMOTE_PATH_CHARS - 1));
        assert_eq!(at_limit.chars().count(), MAX_REMOTE_PATH_CHARS);
        assert!(validate_remote_path(&at_limit).is_ok());
        assert!(validate_remote_path(&format!("{at_limit}x")).is_err());
    }

    #[test]
    fn agent_file_tools_reject_credential_and_process_secret_paths() {
        for path in [
            "/home/deploy/.ssh/id_ed25519",
            "/srv/app/.env.production",
            "/run/secrets/provider-token",
            "/proc/123/environ",
            "/etc/ssl/private/service.key",
            "/home/deploy/.kube/config",
        ] {
            assert!(is_agent_blocked_path(path), "{path}");
        }
        assert!(!is_agent_blocked_path("/srv/app/.env.example"));
        assert!(!is_agent_blocked_path("/etc/app/config.json"));
    }

    #[test]
    fn every_blocked_path_fragment_is_a_rule_of_its_own() {
        // 逐条列出规则片段的目标，漏掉一条就会在这里现形（而不是等某个工具放行）。
        let cases = [
            ("/.aws/", "/home/deploy/.aws/credentials"),
            ("/.azure/", "/home/deploy/.azure/accessTokens.json"),
            (
                "/.config/gcloud/",
                "/home/deploy/.config/gcloud/credentials.db",
            ),
            ("/.kube/", "/home/deploy/.kube/config"),
            ("/.ssh/", "/home/deploy/.ssh/config"),
            ("/proc/", "/proc/self/environ"),
            ("/run/secrets/", "/run/secrets/db-password"),
            ("/var/run/secrets/", "/var/run/secrets/kubernetes.io/token"),
        ];
        for (fragment, path) in cases {
            assert!(is_agent_blocked_path(path), "{fragment} -> {path}");
        }
        // 带尾斜杠的目录目标本身也要拦（末段为空名字，靠片段规则拦住）。
        assert!(is_agent_blocked_path("/home/deploy/.ssh/"));
        // 只是共享前缀的目录不在此列。
        assert!(!is_agent_blocked_path(
            "/home/deploy/.sshfs-cache/notes.txt"
        ));
    }

    #[test]
    fn every_blocked_file_name_and_extension_is_a_rule_of_its_own() {
        for name in [
            "shadow",
            "gshadow",
            "sudoers",
            "id_rsa",
            "id_dsa",
            "id_ecdsa",
            "id_ed25519",
            "credentials",
            "credentials.json",
            "secrets",
            "secrets.json",
        ] {
            assert!(is_agent_blocked_path(&format!("/etc/{name}")), "{name}");
        }
        for name in [
            "server.pem",
            "service.key",
            "client.p12",
            "client.pfx",
            "trust.jks",
        ] {
            assert!(is_agent_blocked_path(&format!("/etc/ssl/{name}")), "{name}");
        }
        // 同名的「看起来像」的路径不受影响。
        assert!(!is_agent_blocked_path("/srv/app/credentials.md"));
        assert!(!is_agent_blocked_path("/srv/app/keyring.json"));
    }

    #[test]
    fn env_templates_are_the_only_env_exception() {
        assert!(is_agent_blocked_path("/srv/app/.env"));
        assert!(is_agent_blocked_path("/srv/app/.env.local"));
        for name in [".env.example", ".env.sample", ".env.template"] {
            assert!(
                !is_agent_blocked_path(&format!("/srv/app/{name}")),
                "{name}"
            );
        }
        // `.environment` 不是 `.env` 家族（没有点分隔的后缀），也不是例外表里的名字。
        assert!(!is_agent_blocked_path("/srv/app/.environment"));
    }

    #[test]
    fn the_whole_policy_is_case_insensitive() {
        for path in [
            "/HOME/DEPLOY/.SSH/ID_RSA",
            "/Srv/App/.ENV.Production",
            "/etc/ssl/private/Service.KEY",
            "/run/SECRETS/provider-token",
            "/etc/SHADOW",
        ] {
            assert!(is_agent_blocked_path(path), "{path}");
        }
        // 例外同样大小写不敏感：判定发生在归一化之后。
        assert!(!is_agent_blocked_path("/srv/app/.ENV.EXAMPLE"));
    }
}

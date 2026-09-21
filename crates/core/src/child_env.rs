//! 派生给子进程的**最小环境**：MCP stdio 服务与 sidecar 共用。
//!
//! 之前两者都直接继承宿主的整个环境。对一个 stdio MCP 服务来说，那等于「用桌面端持有的
//! 每一份凭据去运行一段第三方本地代码」：`SSH_AUTH_SOCK`、云厂商密钥、`GITHUB_TOKEN`、
//! 数据库 DSN 全都对子进程可读，并且可能顺着一次崩溃诊断、一段 stderr 尾部或一条模型
//! 消息泄漏出去。sidecar 是我们自己的代码，但它是会加载第三方包的 Node 进程，所以用同一
//! 套规则。
//!
//! 白名单刻意只放「进程启动与找到自己工具」需要的东西，不放任何凭据。调用方通过
//! `config.env` 显式传入的变量会追加在白名单之上（并可以覆盖同名项），所以真正的集成仍然
//! 可以被有意配置，而不是靠继承。
//!
//! 这是一条**变量名**白名单，不是值白名单：`PATH` 本身不是秘密，而白名单漏掉一个名字的
//! 代价是子进程启动失败（可见、可修），黑名单漏掉一个名字的代价是一把密钥出门（不可见）。

/// 允许传递给子进程的环境变量名。
///
/// 大小写按各平台的习惯：Windows 的环境变量名不区分大小写，`std::env::var` 会命中
/// `Path`/`PATH` 的任意拼写；Unix 上不存在的名字会被直接过滤掉。
const ALLOWED_ENV_NAMES: &[&str] = &[
    // Unix：进程运行时与本地化。
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TERM",
    "TZ",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_NUMERIC",
    "LC_TIME",
    "TMPDIR",
    // Windows：进程运行时、系统目录、用户目录与路径。
    "SystemRoot",
    "SystemDrive",
    "COMSPEC",
    "PATHEXT",
    "TEMP",
    "TMP",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "PROCESSOR_IDENTIFIER",
    "OS",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMW6432",
];

/// sidecar（**我们自己的**进程）额外需要的、由开发者显式设置的运行时开关。
///
/// 它们是 Node 侧 `readConfig` 在启动时读的环境变量，不是凭据；把它们列出来是因为
/// `env_clear()` 会把它们一并清掉，而它们本来是可用的开发旋钮。第三方 MCP 服务拿不到这些
/// 名字 —— 它们只属于我们自己启动的那个进程。
const SIDECAR_RUNTIME_KNOBS: &[&str] = &[
    "YUKINAL_DATA_DIR",
    "YUKINAL_LOG_LEVEL",
    "YUKINAL_MAX_RUN_MS",
];

/// 当前进程里按白名单取出的最小环境。
#[must_use]
pub fn minimal_child_environment() -> Vec<(String, String)> {
    collect_environment(ALLOWED_ENV_NAMES, |name| std::env::var(name).ok())
}

/// sidecar 的环境：最小白名单 + 上面那几个只属于我们自己进程的旋钮。
#[must_use]
pub fn minimal_sidecar_environment() -> Vec<(String, String)> {
    let mut names = ALLOWED_ENV_NAMES.to_vec();
    names.extend_from_slice(SIDECAR_RUNTIME_KNOBS);
    collect_environment(&names, |name| std::env::var(name).ok())
}

/// 白名单的纯实现：用一个 `lookup` 回答「这个名字在当前进程里是什么值」。
///
/// 拆出来只为可测：测试里给一个「每个名字都有一个值」的 `lookup`，就能断言凭据类名字
/// 一个都不会出现在结果里，而不必真的去改动进程环境（那是全局状态，且在新版 Rust 里是
/// `unsafe`）。
fn collect_environment<F>(names: &[&str], lookup: F) -> Vec<(String, String)>
where
    F: Fn(&str) -> Option<String>,
{
    let mut seen = std::collections::HashSet::new();
    let mut environment = Vec::new();
    for name in names {
        // Windows 上 `PATH` 与 `Path` 是同一个变量：去重避免把同一份值塞两遍。
        let key = name.to_ascii_lowercase();
        if !seen.insert(key) {
            continue;
        }
        if let Some(value) = lookup(name) {
            environment.push(((*name).to_string(), value));
        }
    }
    environment
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个「所有名字都有值」的 lookup：足以证明白名单是按名字过滤的。
    fn everything(name: &str) -> Option<String> {
        Some(format!("value-for-{name}"))
    }

    #[test]
    fn credentials_and_development_tokens_never_reach_a_child() {
        let environment = collect_environment(ALLOWED_ENV_NAMES, everything);
        let names: Vec<&str> = environment.iter().map(|(name, _)| name.as_str()).collect();
        for secret in [
            "SSH_AUTH_SOCK",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_ACCESS_KEY_ID",
            "GITHUB_TOKEN",
            "GH_TOKEN",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "DATABASE_URL",
            "NPM_TOKEN",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "KUBECONFIG",
            "DOCKER_CONFIG",
        ] {
            assert!(
                !names.contains(&secret),
                "{secret} must not be inherited by a child process"
            );
        }
    }

    #[test]
    fn the_process_runtime_basics_are_still_present() {
        let environment = collect_environment(ALLOWED_ENV_NAMES, everything);
        let names: Vec<&str> = environment.iter().map(|(name, _)| name.as_str()).collect();
        // 子进程要能找到自己的工具、临时目录与系统目录。
        for required in ["PATH", "TEMP", "SystemRoot", "HOME"] {
            assert!(
                names.contains(&required),
                "{required} is a process runtime basic and must survive the allowlist"
            );
        }
    }

    #[test]
    fn an_absent_name_is_simply_omitted() {
        // 平台差异就是这样被吸收的：Unix 上不存在 SystemRoot，它不是错误。
        let environment = collect_environment(ALLOWED_ENV_NAMES, |name| {
            (name == "PATH").then(|| "/bin".into())
        });
        assert_eq!(
            environment,
            vec![("PATH".to_string(), "/bin".to_string())],
            "only names the host actually has should be emitted"
        );
    }

    #[test]
    fn the_sidecar_keeps_its_documented_runtime_knobs() {
        let mut names = ALLOWED_ENV_NAMES.to_vec();
        names.extend_from_slice(SIDECAR_RUNTIME_KNOBS);
        let environment = collect_environment(&names, everything);
        let emitted: Vec<&str> = environment.iter().map(|(name, _)| name.as_str()).collect();
        // 这几个名字是 sidecar 的启动读项，env_clear() 不该把它们一起清掉。
        for knob in SIDECAR_RUNTIME_KNOBS {
            assert!(emitted.contains(knob), "{knob} is a sidecar startup knob");
        }
    }
}

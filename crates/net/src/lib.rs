//! 出站 HTTP 的代理：读哪些系统设置、怎么接到 HTTP 客户端上。设计取舍见 ADR 0022。
//!
//! 这个 crate 刻意保持小而独立（只依赖 `serde` 与 `reqwest`）：它同时被 `yukinal-core`
//! （MCP 的 Streamable HTTP）、`yukinal-ssh`（远端 KRL 下载）与桌面宿主（OAuth）使用，
//! 而 core 依赖 ssh —— 代理这件事不能塞进两者中的任何一个。
//!
//! 两条不变的性质：
//!
//! - **直连是显式的。** 选直连时客户端会被明确设成 `no_proxy()`，所以环境变量里的代理也
//!   不会被 HTTP 客户端库顺手用上。
//! - **读到了但用不了的配置是失败，不是直连。** 只配了 PAC、URL 里带凭据、scheme 不认识，
//!   都会带着理由拒绝这次连接：静默直连正是「绕过公司代理」那件事。

use reqwest::ClientBuilder;
use serde::{Deserialize, Serialize};

/// 用户在设置里选的模式。
///
/// 默认直连：装上代理软件不该悄悄改变应用的连接路径。要经代理的用户显式选「系统代理」。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkProxyMode {
    #[default]
    Direct,
    System,
}

/// 这次连接到底怎么走。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NetworkProxy {
    /// 直连，且明确关闭客户端库自己的环境变量探测。
    #[default]
    Direct,
    /// 经这个代理。
    Proxy {
        url: String,
        /// `NO_PROXY` / `ProxyOverride` 的原始字符串，匹配交给 HTTP 客户端库。
        no_proxy: Option<String>,
        source: ProxySource,
    },
    /// 读到了配置但不能用：只配了 PAC、URL 非法、内嵌凭据、scheme 不支持……
    Unusable { reason: String },
}

/// 一次出站连接要用的代理材料：解析结果 + 凭据（如果代理要认证）。
///
/// 打包成一个值，是因为三个调用方（MCP 传输、SSH 的 KRL 下载、OAuth）必须用**同一份**
/// 设置：一份解析结果分头再查一次环境变量，就会出现「同一个进程里两条请求走了两条路」。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutboundProxy {
    pub proxy: NetworkProxy,
    pub credential: Option<ProxyCredential>,
}

/// 代理是从哪里读来的。设置页要能回答「这次走了哪条路、谁定的」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxySource {
    EnvironmentVariable,
    /// Windows：`HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings`。
    WindowsInternetSettings,
    /// macOS：`scutil --proxy`。
    MacOsSystemConfiguration,
}

impl ProxySource {
    pub fn describe(self) -> &'static str {
        match self {
            ProxySource::EnvironmentVariable => "环境变量",
            ProxySource::WindowsInternetSettings => "Windows Internet 设置",
            ProxySource::MacOsSystemConfiguration => "macOS 网络设置",
        }
    }
}

/// 代理凭据（`user:password`）。
///
/// 手写 `Debug`：这个值会跟着配置结构出现在任何地方，而一个 derive 出来的 `Debug`
/// 正是它泄漏的方式。它只进系统凭据库，也只出现在 `Proxy-Authorization` 上。
#[derive(Clone, PartialEq, Eq)]
pub struct ProxyCredential(String);

impl ProxyCredential {
    /// 校验并收下一个凭据。用户名里不能有冒号（那会切开两半），整串不能有控制字符。
    pub fn new(value: String) -> Result<Self, String> {
        split_credential(&value)?;
        if value.len() > 1_024 {
            return Err("proxy credential is longer than 1024 characters".to_string());
        }
        Ok(Self(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ProxyCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ProxyCredential(<redacted>)")
    }
}

/// 按模式解析出这次连接要怎么走。
///
/// 选「系统代理」时先看环境变量，再看平台设置：命令行与容器里通常只有前者，而 Windows
/// 用户大多只在「Internet 选项」里配过。
pub fn resolve(mode: NetworkProxyMode) -> NetworkProxy {
    if mode == NetworkProxyMode::Direct {
        return NetworkProxy::Direct;
    }
    from_environment()
        .or_else(platform_proxy)
        .unwrap_or(NetworkProxy::Direct)
}

/// 把解析结果装到一个 HTTP 客户端上。
///
/// `credential` 是 `user:password`（来自系统凭据库，调用方负责取）；它只会出现在
/// `Proxy-Authorization` 上，不进日志。
pub fn apply(
    builder: ClientBuilder,
    proxy: &NetworkProxy,
    credential: Option<&ProxyCredential>,
) -> Result<ClientBuilder, String> {
    match proxy {
        NetworkProxy::Direct => Ok(builder.no_proxy()),
        NetworkProxy::Unusable { reason } => Err(reason.clone()),
        NetworkProxy::Proxy { url, no_proxy, .. } => {
            let url = normalized(url)?;
            let mut configured = reqwest::Proxy::all(url.clone())
                .map_err(|_| "代理地址无法用于 HTTP 客户端".to_string())?;
            if let Some(credential) = credential {
                let (user, password) = split_credential(credential.expose())?;
                configured = configured.basic_auth(user, password);
            }
            if let Some(no_proxy) = no_proxy.as_deref().and_then(reqwest::NoProxy::from_string) {
                configured = configured.no_proxy(Some(no_proxy));
            }
            Ok(builder.proxy(configured))
        }
    }
}

/// Add the selected route to a user-facing transport error without exposing credentials.
///
/// The route is part of the diagnosis: the same connection failure means something different
/// when the request went directly to the endpoint or stopped at a configured proxy.
pub fn route_context(proxy: &NetworkProxy, reason: &str) -> String {
    match proxy {
        NetworkProxy::Direct => format!("{reason} (direct connection)"),
        NetworkProxy::Proxy { url, source, .. } => {
            let display_url = normalized(url).unwrap_or_else(|_| "<invalid proxy URL>".to_string());
            format!(
                "{reason} (via proxy {display_url}, from {})",
                source.describe()
            )
        }
        NetworkProxy::Unusable {
            reason: proxy_reason,
        } => format!("{reason} (proxy configuration unusable: {proxy_reason})"),
    }
}

/// `user:password` → 两半。密码里的冒号是合法的，用户名里的不是。
fn split_credential(credential: &str) -> Result<(&str, &str), String> {
    let (user, password) = credential
        .split_once(':')
        .ok_or_else(|| "proxy credential must be `user:password`".to_string())?;
    if user.is_empty() || credential.chars().any(char::is_control) {
        return Err(
            "proxy credential must be `user:password` without control characters".to_string(),
        );
    }
    Ok((user, password))
}

/// 环境变量里的代理。大小写都认：`curl` 时代两种写法都用，脚本里两种都出现过。
fn from_environment() -> Option<NetworkProxy> {
    from_environment_with(env_of)
}

fn from_environment_with(get: impl Fn(&str) -> Option<String>) -> Option<NetworkProxy> {
    let raw = [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ]
    .iter()
    .find_map(|name| non_empty(get(name)))?;
    let no_proxy = ["NO_PROXY", "no_proxy"]
        .iter()
        .find_map(|name| non_empty(get(name)));
    Some(match normalized(&raw) {
        Ok(url) => NetworkProxy::Proxy {
            url,
            no_proxy,
            source: ProxySource::EnvironmentVariable,
        },
        Err(reason) => NetworkProxy::Unusable {
            reason: format!("{reason}（来自环境变量）"),
        },
    })
}

fn env_of(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

/// 把读到的代理值变成可以交给 HTTP 客户端的 URL。
///
/// 规则与 endpoint 的 URL 规则一致：只认 `http`/`https`，拒绝内嵌凭据、query 与 fragment ——
/// 代理凭据属于系统凭据库，不属于一个可以被打印出来的字符串。
fn normalized(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("代理地址是空的".to_string());
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        // Windows 的 `ProxyServer` 常见写法就是 `host:port`。
        format!("http://{trimmed}")
    };
    let parsed =
        reqwest::Url::parse(&with_scheme).map_err(|_| "代理地址不是合法 URL".to_string())?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(format!(
                "代理地址的 scheme `{other}` 不支持；只支持 http 与 https \
                 （SOCKS 需要另一套协议栈）"
            ))
        }
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(
            "代理地址里内嵌了用户名或密码；请把凭据填在「代理凭据」里，它只进系统凭据库"
                .to_string(),
        );
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("代理地址不能带 query 或 fragment".to_string());
    }
    if parsed.host_str().is_none() {
        return Err("代理地址没有主机名".to_string());
    }
    Ok(parsed.to_string().trim_end_matches('/').to_string())
}

/// 平台设置里读到的原始值。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PlatformProxy {
    /// 静态代理；`None` 表示这台机器没配静态代理。
    static_proxy: Option<String>,
    no_proxy: Option<String>,
    /// PAC 的地址；我们不执行它，但要说清楚「为什么没有代理可用」。
    pac_url: Option<String>,
}

/// 按平台读系统代理设置。
fn platform_proxy() -> Option<NetworkProxy> {
    let platform = read_platform_proxy()?;
    if let Some(raw) = platform.static_proxy.as_deref() {
        return Some(match normalized(raw) {
            Ok(url) => NetworkProxy::Proxy {
                url,
                no_proxy: platform.no_proxy.clone(),
                source: platform_source(),
            },
            Err(reason) => NetworkProxy::Unusable {
                reason: format!("{reason}（来自{}）", platform_source().describe()),
            },
        });
    }
    if platform.pac_url.is_some() {
        return Some(NetworkProxy::Unusable {
            reason: "系统代理只配置了 PAC：PAC 是一段 JavaScript，本应用不执行它。\
                 请在设置里选「直连」，或把静态代理写进 HTTPS_PROXY 环境变量"
                .to_string(),
        });
    }
    // 平台说「没有代理」：直连是正确的答案，不是失败。
    Some(NetworkProxy::Direct)
}

fn platform_source() -> ProxySource {
    #[cfg(windows)]
    {
        ProxySource::WindowsInternetSettings
    }
    #[cfg(target_os = "macos")]
    {
        ProxySource::MacOsSystemConfiguration
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        ProxySource::EnvironmentVariable
    }
}

#[cfg(windows)]
fn read_platform_proxy() -> Option<PlatformProxy> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    use winreg::RegKey;

    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(
            r"Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            KEY_READ,
        )
        .ok()?;
    let enabled: u32 = key.get_value("ProxyEnable").unwrap_or(0);
    let server: String = key.get_value("ProxyServer").unwrap_or_default();
    let overrides: String = key.get_value("ProxyOverride").unwrap_or_default();
    let pac: String = key.get_value("AutoConfigURL").unwrap_or_default();
    let auto_detect: u32 = key.get_value("AutoDetect").unwrap_or(0);
    Some(PlatformProxy {
        static_proxy: (enabled == 1)
            .then(|| parse_windows_proxy_server(&server))
            .flatten(),
        no_proxy: windows_no_proxy(&overrides),
        pac_url: non_empty(Some(pac))
            .or_else(|| (auto_detect == 1).then(|| "automatic proxy discovery".to_string())),
    })
}

#[cfg(target_os = "macos")]
fn read_platform_proxy() -> Option<PlatformProxy> {
    let output = std::process::Command::new("scutil")
        .arg("--proxy")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_scutil_proxy(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn read_platform_proxy() -> Option<PlatformProxy> {
    // Linux 上「系统代理」没有唯一答案（GNOME/KDE/环境变量各一套），这里只认环境变量。
    None
}

/// Windows 的 `ProxyServer` 有两种形状：`host:port`，或按 scheme 分列的
/// `http=host:port;https=host:port;ftp=…`。取 https，其次 http，其次第一个非空项。
fn parse_windows_proxy_server(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed.contains('=') {
        return Some(trimmed.to_string());
    }
    let mut fallback = None;
    for entry in trimmed.split(';') {
        let Some((scheme, value)) = entry.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match scheme.trim().to_ascii_lowercase().as_str() {
            "https" => return Some(format!("https://{value}")),
            "http" => fallback = Some(format!("http://{value}")),
            _ => {
                if fallback.is_none() {
                    fallback = Some(format!("http://{value}"));
                }
            }
        }
    }
    fallback
}

/// `ProxyOverride` 用 `;` 分隔，并且可以有 `<local>`（本机短名）。我们不做「本机」那套
/// 判断，把 `<local>` 丢掉、其余原样交给客户端库的 `NO_PROXY` 匹配。
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_no_proxy(override_list: &str) -> Option<String> {
    let entries: Vec<&str> = override_list
        .split(';')
        .map(str::trim)
        .filter(|entry| !entry.is_empty() && !entry.eq_ignore_ascii_case("<local>"))
        .collect();
    (!entries.is_empty()).then(|| entries.join(","))
}

/// `scutil --proxy` 输出的是一个带缩进的字典文本，逐行认键就够了。
///
/// 形如：
/// ```text
/// <dictionary> {
///   ExceptionsList : <array> {
///     0 : *.local
///   }
///   HTTPEnable : 1
///   HTTPPort : 8080
///   HTTPProxy : proxy.example.com
///   HTTPSEnable : 1
///   HTTPSPort : 8080
///   HTTPSProxy : proxy.example.com
///   ProxyAutoConfigEnable : 0
///   ProxyAutoConfigURLString : http://wpad/wpad.dat
/// }
/// ```
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_scutil_proxy(output: &str) -> Option<PlatformProxy> {
    let mut fields: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    let mut exceptions: Vec<&str> = Vec::new();
    let mut in_exceptions = false;
    for line in output.lines() {
        let line = line.trim();
        if line.starts_with("ExceptionsList") {
            in_exceptions = true;
            continue;
        }
        if in_exceptions {
            if line == "}" {
                in_exceptions = false;
                continue;
            }
            if let Some((_, value)) = line.split_once(':') {
                exceptions.push(value.trim());
            }
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            fields.insert(key.trim(), value.trim());
        }
    }
    let enabled = |name: &str| fields.get(name).is_some_and(|value| *value == "1");
    let proxy = |host: &str, port: &str| {
        let host = fields
            .get(host)
            .copied()
            .filter(|value| !value.is_empty())?;
        match fields.get(port).and_then(|value| value.parse::<u16>().ok()) {
            Some(port) => Some(format!("http://{host}:{port}")),
            None => Some(format!("http://{host}")),
        }
    };
    let static_proxy = if enabled("HTTPSEnable") {
        proxy("HTTPSProxy", "HTTPSPort").or_else(|| proxy("HTTPProxy", "HTTPPort"))
    } else if enabled("HTTPEnable") {
        proxy("HTTPProxy", "HTTPPort")
    } else {
        None
    };
    let pac_url = enabled("ProxyAutoConfigEnable")
        .then(|| fields.get("ProxyAutoConfigURLString").copied())
        .flatten()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            enabled("ProxyAutoDiscoveryEnable").then(|| "automatic proxy discovery".to_string())
        });
    Some(PlatformProxy {
        static_proxy,
        no_proxy: (!exceptions.is_empty()).then(|| exceptions.join(",")),
        pac_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_windows_proxy_server_is_read_in_both_shapes() {
        assert_eq!(
            parse_windows_proxy_server("proxy.corp:8080").as_deref(),
            Some("proxy.corp:8080")
        );
        assert_eq!(
            parse_windows_proxy_server("http=proxy-a:3128;https=proxy-b:8443").as_deref(),
            Some("https://proxy-b:8443"),
            "https 的那条优先：MCP 的 endpoint 必须是 HTTPS"
        );
        assert_eq!(
            parse_windows_proxy_server("ftp=proxy-c:21;http=proxy-d:3128").as_deref(),
            Some("http://proxy-d:3128"),
            "没有 https 时退到 http"
        );
        assert_eq!(parse_windows_proxy_server("   "), None);
    }

    #[test]
    fn windows_proxy_overrides_become_a_no_proxy_list() {
        assert_eq!(
            windows_no_proxy("*.corp;10.*;<local>").as_deref(),
            Some("*.corp,10.*")
        );
        assert_eq!(windows_no_proxy("<local>"), None);
        assert_eq!(windows_no_proxy(""), None);
    }

    #[test]
    fn scutil_output_is_read_like_the_report_it_is() {
        let output = "<dictionary> {\n  ExceptionsList : <array> {\n    0 : *.local\n    1 : 10.0/8\n  }\n  HTTPEnable : 1\n  HTTPPort : 8080\n  HTTPProxy : proxy.corp\n  HTTPSEnable : 1\n  HTTPSPort : 8443\n  HTTPSProxy : secure.corp\n  ProxyAutoConfigEnable : 0\n}\n";
        let parsed = parse_scutil_proxy(output).expect("parsed");
        assert_eq!(
            parsed.static_proxy.as_deref(),
            Some("http://secure.corp:8443")
        );
        assert_eq!(parsed.no_proxy.as_deref(), Some("*.local,10.0/8"));
        assert_eq!(parsed.pac_url, None);
    }

    #[test]
    fn a_pac_only_configuration_is_reported_as_unusable() {
        let output = "<dictionary> {\n  HTTPEnable : 0\n  ProxyAutoConfigEnable : 1\n  ProxyAutoConfigURLString : http://wpad/wpad.dat\n}\n";
        let parsed = parse_scutil_proxy(output).expect("parsed");
        assert_eq!(parsed.static_proxy, None);
        assert_eq!(parsed.pac_url.as_deref(), Some("http://wpad/wpad.dat"));
    }

    #[test]
    fn automatic_pac_discovery_is_not_treated_as_direct() {
        let output = "<dictionary> {\n  HTTPEnable : 0\n  ProxyAutoDiscoveryEnable : 1\n}\n";
        let parsed = parse_scutil_proxy(output).expect("parsed");
        assert_eq!(parsed.static_proxy, None);
        assert_eq!(parsed.pac_url.as_deref(), Some("automatic proxy discovery"));
    }

    #[test]
    fn environment_proxy_priority_prefers_https_then_http_then_all() {
        let resolved = from_environment_with(|name| match name {
            "HTTPS_PROXY" => None,
            "https_proxy" => None,
            "HTTP_PROXY" => Some("http://http-proxy:8080".to_string()),
            "http_proxy" => None,
            "ALL_PROXY" => Some("http://all-proxy:1080".to_string()),
            "all_proxy" => None,
            _ => None,
        })
        .expect("HTTP_PROXY should be selected before ALL_PROXY");
        assert!(matches!(
            resolved,
            NetworkProxy::Proxy { ref url, .. } if url == "http://http-proxy:8080"
        ));
    }

    #[test]
    fn proxy_urls_are_normalized_the_same_way_endpoints_are() {
        assert_eq!(
            normalized("proxy.corp:3128").expect("host:port"),
            "http://proxy.corp:3128"
        );
        assert_eq!(
            normalized("http://proxy.corp:3128/").expect("trailing slash"),
            "http://proxy.corp:3128"
        );
        assert!(normalized("socks5://proxy.corp:1080")
            .expect_err("socks")
            .contains("不支持"));
        let scheme_error = normalized("socks5://user:secret@proxy.corp:1080")
            .expect_err("socks with embedded credentials");
        assert!(!scheme_error.contains("secret"), "{scheme_error}");
        assert!(normalized("http://user:secret@proxy.corp:3128")
            .expect_err("credentials in the URL")
            .contains("内嵌"));
        let credential_error =
            normalized("http://user:secret@proxy.corp:3128").expect_err("credentials in the URL");
        assert!(!credential_error.contains("secret"), "{credential_error}");
        assert!(normalized("http://proxy.corp:3128/?x=1")
            .expect_err("query")
            .contains("query"));
        assert!(normalized("").expect_err("empty").contains("空"));
    }

    #[test]
    fn credentials_are_split_at_the_first_colon() {
        assert_eq!(
            split_credential("corp\\user:p:a:ss").expect("split"),
            ("corp\\user", "p:a:ss")
        );
        assert!(split_credential("no-colon").is_err());
        assert!(split_credential(":secret").is_err());
        assert!(split_credential("user:sec\nret").is_err());
    }

    #[test]
    fn direct_mode_never_reads_the_environment() {
        // 直连是默认值，也是唯一不看环境变量的分支：它必须明确关掉客户端库自己的探测。
        assert_eq!(resolve(NetworkProxyMode::Direct), NetworkProxy::Direct);
    }

    #[test]
    fn a_credential_is_redacted_in_debug_and_rejects_control_characters() {
        let credential = ProxyCredential::new("corp\\user:p@ss".to_string()).expect("valid");
        assert_eq!(credential.expose(), "corp\\user:p@ss");
        let debug = format!("{credential:?}");
        assert!(!debug.contains("p@ss"), "{debug}");
        assert!(debug.contains("redacted"));
        assert!(ProxyCredential::new("no-colon".to_string()).is_err());
        assert!(ProxyCredential::new("user:pa\nss".to_string()).is_err());
    }

    #[test]
    fn route_context_distinguishes_direct_and_proxy_failures_without_secrets() {
        let direct = route_context(&NetworkProxy::Direct, "connection refused");
        assert!(direct.contains("direct connection"), "{direct}");

        let proxy = NetworkProxy::Proxy {
            url: "http://proxy.corp:3128".to_string(),
            no_proxy: None,
            source: ProxySource::EnvironmentVariable,
        };
        let through_proxy = route_context(&proxy, "connection refused");
        assert!(
            through_proxy.contains("via proxy http://proxy.corp:3128"),
            "{through_proxy}"
        );
        assert!(through_proxy.contains("环境变量"), "{through_proxy}");
        assert!(!through_proxy.contains("secret"), "{through_proxy}");

        let malformed = NetworkProxy::Proxy {
            url: "http://user:secret@proxy.corp:3128".to_string(),
            no_proxy: None,
            source: ProxySource::EnvironmentVariable,
        };
        let malformed_context = route_context(&malformed, "connection refused");
        assert!(!malformed_context.contains("secret"), "{malformed_context}");
        assert!(
            malformed_context.contains("invalid proxy URL"),
            "{malformed_context}"
        );
    }
}

//! MCP configuration and view projection.
//!
//! The command module owns the IPC entry points; this module owns the invariant-heavy
//! translation between editable settings, credential references and core transport config.

use super::*;

pub(super) fn resolve_oauth_config(
    server_id: &str,
    is_http: bool,
    requested: Option<McpOAuthInput>,
    existing: Option<McpOAuthConfig>,
    http_auth_headers: &[McpHttpAuthHeaderConfig],
    credentials: &dyn CredentialStore,
) -> Result<(Option<McpOAuthConfig>, Vec<CredentialRef>), String> {
    let Some(requested) = requested else {
        return Ok((None, Vec::new()));
    };
    if !is_http {
        return Err("OAuth settings require the HTTP transport".to_string());
    }
    if http_auth_headers
        .iter()
        .any(|header| header.name.eq_ignore_ascii_case("authorization"))
    {
        return Err(
            "OAuth and a static Authorization header cannot be configured together".to_string(),
        );
    }
    let raw_issuer = requested.issuer.trim();
    let issuer = if raw_issuer.is_empty() {
        String::new()
    } else {
        yukinal_core::mcp::validate_oauth_url(server_id, raw_issuer)
            .map_err(|error| format!("无效的 OAuth issuer：{error}"))?
            .trim_end_matches('/')
            .to_string()
    };
    let client_id = requested.client_id.trim();
    if client_id.len() > 512 || client_id.chars().any(char::is_control) {
        return Err("OAuth client id must be at most 512 printable characters".to_string());
    }
    let flow = requested.flow.unwrap_or_default();
    let client_auth = requested.client_auth.unwrap_or_default();
    if client_auth.needs_secret() && client_id.is_empty() {
        return Err(
            "客户端密钥认证需要手填 client id：动态注册得到的是一个公共客户端，\
             服务端即使返回 secret 也不会被使用。"
                .to_string(),
        );
    }
    if client_auth == McpOAuthClientAuth::ClientSecretBasic && client_id.contains(':') {
        return Err(
            "client_secret_basic 的 client id 不能包含冒号：RFC 7617 用它分隔用户名与密码。"
                .to_string(),
        );
    }
    if requested.scopes.len() > 32 {
        return Err("OAuth may request at most 32 scopes".to_string());
    }
    let mut scopes = Vec::new();
    for scope in requested.scopes {
        let scope = scope.trim();
        if scope.is_empty()
            || scope.len() > 128
            || scope
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
        {
            return Err("each OAuth scope must be 1 to 128 non-whitespace characters".to_string());
        }
        if !scopes.iter().any(|existing| existing == scope) {
            scopes.push(scope.to_string());
        }
    }
    let preserve = existing.as_ref().is_some_and(|existing| {
        !issuer.is_empty()
            && existing.issuer.trim_end_matches('/') == issuer.trim_end_matches('/')
            && existing.client_id == client_id
            // The flow is part of the identity, not a display preference: a token obtained
            // through the browser redirect is not automatically the one this flow would
            // ask for, so switching forces a fresh authorization instead of reusing it.
            && existing.flow == flow
            && existing.client_auth == client_auth
            // 打开/关掉发送方约束同样是换身份：旧令牌没有绑定这把钥匙（或绑的是另一把），
            // 留着它只会让下一次请求以一种说不清的方式失败。
            && existing.dpop == requested.dpop
            && existing.scopes == scopes
    });
    // The secret is write-only, so "no new value" means one of two things: keep the one
    // that is already stored (same method), or refuse. It never means "store an empty
    // secret" — and switching the method drops the old secret, because the user is telling
    // us the previous way of authenticating was wrong.
    let mut staged = Vec::new();
    let client_secret_ref = if !client_auth.needs_secret() {
        None
    } else {
        let entered = match requested.client_secret.filter(|value| !value.is_empty()) {
            Some(secret) => {
                if secret.len() > 8_192 || secret.chars().any(char::is_control) {
                    return Err(
                        "OAuth client secret 必须是 1 到 8192 个不含控制字符的字符".to_string()
                    );
                }
                let reference = credentials
                    .set(
                        "mcp",
                        &format!("{server_id}-{}", next_id("oauth-secret")),
                        &Secret::from_utf8(secret),
                    )
                    .map_err(|error| format!("保存 OAuth client secret 失败：{error}"))?;
                staged.push(reference.clone());
                Some(reference.to_string_ref())
            }
            None => None,
        };
        // Keeping the stored secret is only legitimate for the *same* method: switching
        // methods reclaims the old secret, so the user has to enter the new one.
        let carried = existing
            .as_ref()
            .filter(|oauth| oauth.client_auth == client_auth)
            .and_then(|oauth| oauth.client_secret_ref.clone());
        Some(match entered.or(carried) {
            Some(reference) => reference,
            None => return Err(
                "这种客户端认证方式需要一个 client secret，而这次没有输入新的，也没有可保留的。\
                     请填入 secret（切换认证方式会让旧 secret 失效）。"
                    .to_string(),
            ),
        })
    };
    Ok((
        Some(McpOAuthConfig {
            issuer,
            client_id: client_id.to_string(),
            flow,
            client_auth,
            client_secret_ref,
            dpop: requested.dpop,
            // 密钥引用只在「同一个身份 + 仍然要 DPoP」时沿用：换身份等于旧令牌作废，
            // 那把钥匙也就没有对手了，回收掉比留着更像话（下一次连接会生成新的）。
            dpop_key_ref: preserve
                .then(|| {
                    existing
                        .as_ref()
                        .and_then(|oauth| oauth.dpop_key_ref.clone())
                })
                .flatten(),
            scopes,
            token_endpoint: preserve
                .then(|| {
                    existing
                        .as_ref()
                        .and_then(|oauth| oauth.token_endpoint.clone())
                })
                .flatten(),
            credential_ref: preserve
                .then(|| {
                    existing
                        .as_ref()
                        .and_then(|oauth| oauth.credential_ref.clone())
                })
                .flatten(),
        }),
        staged,
    ))
}

pub(super) fn resolve_http_auth_headers(
    database: &Database,
    server_id: &str,
    existing: &[McpHttpAuthHeaderConfig],
    requested: Vec<McpHttpAuthHeaderInput>,
    credentials: &dyn CredentialStore,
) -> Result<(Vec<McpHttpAuthHeaderConfig>, Vec<CredentialRef>), String> {
    if requested.len() > yukinal_core::mcp::MAX_HTTP_AUTH_HEADERS {
        return Err(format!(
            "一个 HTTP endpoint 最多配置 {} 个认证头。",
            yukinal_core::mcp::MAX_HTTP_AUTH_HEADERS
        ));
    }
    let mut seen = HashSet::new();
    let mut resolved = Vec::with_capacity(requested.len());
    let mut staged = Vec::new();
    for header in requested {
        let name = header.name.trim();
        if name.is_empty() {
            delete_credentials(database, credentials, &staged);
            return Err("HTTP 认证头需要一个非空名称。".to_string());
        }
        if !seen.insert(name.to_ascii_lowercase()) {
            delete_credentials(database, credentials, &staged);
            return Err(format!("HTTP 认证头 `{name}` 重复。"));
        }
        let secret = header.secret.filter(|value| !value.is_empty());
        if let Err(error) = McpHttpAuthHeader::new(name, secret.as_deref().unwrap_or("placeholder"))
        {
            delete_credentials(database, credentials, &staged);
            return Err(format!("无效的 HTTP 认证头 `{name}`：{error}"));
        }
        if let Some(secret) = secret {
            let reference = match credentials.set(
                "mcp",
                &format!("{server_id}-{}", next_id("cred")),
                &Secret::from_utf8(secret),
            ) {
                Ok(reference) => reference,
                Err(error) => {
                    delete_credentials(database, credentials, &staged);
                    return Err(format!("保存 MCP HTTP 凭据失败：{error}"));
                }
            };
            staged.push(reference.clone());
            resolved.push(McpHttpAuthHeaderConfig {
                name: name.to_string(),
                credential_ref: reference.to_string_ref(),
            });
            continue;
        }
        let Some(existing) = existing
            .iter()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(name))
        else {
            delete_credentials(database, credentials, &staged);
            return Err(format!(
                "HTTP 认证头 `{name}` 没有新 secret，也没有可保留的现有凭据。"
            ));
        };
        resolved.push(McpHttpAuthHeaderConfig {
            name: name.to_string(),
            credential_ref: existing.credential_ref.clone(),
        });
    }
    Ok((resolved, staged))
}

pub(super) fn delete_credentials(
    database: &Database,
    credentials: &dyn CredentialStore,
    references: &[CredentialRef],
) {
    for reference in references {
        if let Err(error) =
            crate::state::credential_cleanup::reclaim(database, credentials, reference)
        {
            tracing::warn!(
                "could not immediately reclaim a staged MCP credential; it was queued for retry: {error}"
            );
        }
    }
}

/// 这一行的**传输方式**能不能用。
///
/// 判断不在这里重写：问 [`McpTransportConfig::from_server_config`]。它同时会拒绝一个无法变成
/// 内部名段的 id —— 那种行永远不可能有工具，早一点拒绝比留一个永远不工作的条目好。
///
/// 「还没写完」的错误（禁用、没填 command/url）**允许**保存：草稿是合法的，用户先写下来再补。
pub(super) fn check_transport(config: &McpServerConfig) -> Result<(), String> {
    match McpTransportConfig::from_server_config(config, DEFAULT_REQUEST_TIMEOUT) {
        Ok(_) => Ok(()),
        Err(
            error @ (McpError::UnsupportedTransport { .. }
            | McpError::InvalidUrl { .. }
            | McpError::InvalidConfig { .. }),
        ) => Err(format!(
            "{}。当前支持 stdio 与 Streamable HTTP 传输；远程 HTTP 必须使用 HTTPS，明文 HTTP 只允许回环地址。",
            error
        )),
        Err(_) => Ok(()),
    }
}

pub(super) fn http_auth_config_error(row: &McpServerConfig) -> Option<McpError> {
    if row.transport != "http" && !row.http_auth_headers.is_empty() {
        return Some(McpError::InvalidConfig {
            server_id: row.id.clone(),
            reason: "stdio transport cannot carry HTTP authentication settings".to_string(),
        });
    }
    if row.transport != "http" && row.oauth.is_some() {
        return Some(McpError::InvalidConfig {
            server_id: row.id.clone(),
            reason: "stdio transport cannot carry OAuth authentication settings".to_string(),
        });
    }
    if row.http_auth_headers.len() > yukinal_core::mcp::MAX_HTTP_AUTH_HEADERS {
        return Some(McpError::InvalidConfig {
            server_id: row.id.clone(),
            reason: format!(
                "an HTTP endpoint may carry at most {} authentication headers",
                yukinal_core::mcp::MAX_HTTP_AUTH_HEADERS
            ),
        });
    }
    let mut names = HashSet::new();
    for header in &row.http_auth_headers {
        if header.name.trim().is_empty() || header.credential_ref.trim().is_empty() {
            return Some(McpError::InvalidConfig {
                server_id: row.id.clone(),
                reason: "HTTP authentication headers require a name and credential reference"
                    .to_string(),
            });
        }
        if let Err(reason) = McpHttpAuthHeader::new(&header.name, "placeholder") {
            return Some(McpError::InvalidConfig {
                server_id: row.id.clone(),
                reason,
            });
        }
        if !names.insert(header.name.to_ascii_lowercase()) {
            return Some(McpError::InvalidConfig {
                server_id: row.id.clone(),
                reason: format!(
                    "HTTP authentication header `{}` is configured more than once",
                    header.name
                ),
            });
        }
    }
    if let Some(oauth) = &row.oauth {
        if row
            .http_auth_headers
            .iter()
            .any(|header| header.name.eq_ignore_ascii_case("authorization"))
        {
            return Some(McpError::InvalidConfig {
                server_id: row.id.clone(),
                reason: "OAuth and a static Authorization header are mutually exclusive"
                    .to_string(),
            });
        }
        if !oauth.issuer.trim().is_empty() {
            if let Err(error) = yukinal_core::mcp::validate_oauth_url(&row.id, &oauth.issuer) {
                return Some(error);
            }
        }
        if oauth.client_id.len() > 512 || oauth.client_id.chars().any(char::is_control) {
            return Some(McpError::InvalidConfig {
                server_id: row.id.clone(),
                reason: "OAuth client id is too long or contains control characters".to_string(),
            });
        }
        if oauth.client_auth.needs_secret() {
            if oauth.client_id.trim().is_empty() {
                return Some(McpError::InvalidConfig {
                    server_id: row.id.clone(),
                    reason: "client secret authentication needs a hand-filled client id"
                        .to_string(),
                });
            }
            if oauth.client_secret_ref.is_none() {
                return Some(McpError::InvalidConfig {
                    server_id: row.id.clone(),
                    reason: "OAuth uses client secret authentication but no secret is stored; \
                             edit the server and enter it"
                        .to_string(),
                });
            }
        }
        match (
            oauth.token_endpoint.as_deref(),
            oauth.credential_ref.as_deref(),
        ) {
            (None, None) => {
                return Some(McpError::InvalidConfig {
                    server_id: row.id.clone(),
                    reason: "OAuth is configured but not connected; run Connect OAuth".to_string(),
                })
            }
            (Some(token_endpoint), Some(_)) => {
                if oauth.client_id.trim().is_empty() {
                    return Some(McpError::InvalidConfig {
                        server_id: row.id.clone(),
                        reason: "connected OAuth configuration has no client id".to_string(),
                    });
                }
                if let Err(error) = yukinal_core::mcp::validate_oauth_url(&row.id, token_endpoint) {
                    return Some(error);
                }
            }
            _ => {
                return Some(McpError::InvalidConfig {
                    server_id: row.id.clone(),
                    reason: "OAuth token endpoint and credential reference must both be present"
                        .to_string(),
                })
            }
        }
    }
    None
}

pub(super) fn apply_http_auth(
    row: &McpServerConfig,
    config: &mut McpTransportConfig,
    database: &Database,
    credentials: Arc<dyn CredentialStore>,
) -> Result<(), McpError> {
    if let Some(error) = http_auth_config_error(row) {
        return Err(error);
    }
    let McpTransportConfig::Http(http) = config else {
        return Ok(());
    };
    // 出站代理是应用级设置（ADR 0022）：在这里落地，之后建立连接、发请求都按它走。
    let outbound = crate::commands::network::resolve_outbound_proxy(database, credentials.as_ref())
        .map_err(|reason| McpError::InvalidConfig {
            server_id: row.id.clone(),
            reason,
        })?;
    let mut next = http
        .clone()
        .with_proxy(outbound.proxy.clone(), outbound.credential.clone());
    for header in &row.http_auth_headers {
        let reference = CredentialRef::parse(&header.credential_ref).map_err(|error| {
            McpError::InvalidConfig {
                server_id: row.id.clone(),
                reason: format!("invalid HTTP credential reference: {error}"),
            }
        })?;
        let secret = credentials
            .get(&reference)
            .map_err(|error| McpError::InvalidConfig {
                server_id: row.id.clone(),
                reason: format!("could not read HTTP authentication secret: {error}"),
            })?;
        let secret = secret.as_utf8().map_err(|error| McpError::InvalidConfig {
            server_id: row.id.clone(),
            reason: format!("HTTP authentication secret is not UTF-8: {error}"),
        })?;
        next = next.with_auth_header(&header.name, secret.as_ref())?;
    }
    if let Some(oauth) = &row.oauth {
        let token_endpoint = oauth
            .token_endpoint
            .as_deref()
            .ok_or_else(|| McpError::OAuth {
                server_id: row.id.clone(),
                reason: "OAuth is not connected: no token endpoint is stored".to_string(),
            })?;
        let credential_ref = oauth
            .credential_ref
            .as_deref()
            .ok_or_else(|| McpError::OAuth {
                server_id: row.id.clone(),
                reason: "OAuth is not connected: no token credential is stored".to_string(),
            })?;
        let source = oauth::source_from_config(
            credentials,
            &McpOAuthSourceConfig {
                server_id: row.id.clone(),
                resource: next.url.clone(),
                token_endpoint: token_endpoint.to_string(),
                client_id: oauth.client_id.clone(),
                client_auth: oauth.client_auth,
                client_secret_ref: oauth.client_secret_ref.clone(),
                dpop_key_ref: oauth.dpop_key_ref.clone(),
                proxy: outbound.clone(),
                scopes: oauth.scopes.clone(),
                credential_ref: credential_ref.to_string(),
            },
        )
        .map_err(|reason| McpError::OAuth {
            server_id: row.id.clone(),
            reason,
        })?;
        next = next.with_oauth_source(source)?;
    }
    *http = next;
    Ok(())
}

/// A saved connection change must not leave the supervisor attached to the old target.
pub(super) fn requires_restart(previous: &McpServerConfig, next: &McpServerConfig) -> bool {
    (previous.enabled && !next.enabled)
        || previous.transport != next.transport
        || previous.command != next.command
        || previous.args != next.args
        || previous.url != next.url
        || previous.http_auth_headers != next.http_auth_headers
        || previous.oauth != next.oauth
        // Changing what is trusted changes which annotations the host may honour, so the
        // running process must re-list its tools and the catalog must be rebuilt (ADR 0074).
        || previous.annotation_trust != next.annotation_trust
}

/// 一次「为什么它没在跑」的视图。
pub(super) async fn view_with_unavailable(
    supervisor: &McpSupervisor,
    row: McpServerConfig,
    error: &McpError,
    message: String,
) -> McpServerView {
    let mut view = view(supervisor, row).await;
    view.unavailable = Some(McpServerUnavailable {
        code: McpFailureCode::of(error),
        message,
    });
    view
}

/// 一行 → 界面读数。
///
/// 规则是「跑着就以状态为准，没跑才说为什么」；stdio 与 HTTP 共用同一份状态视图。
pub(super) async fn view(supervisor: &McpSupervisor, row: McpServerConfig) -> McpServerView {
    let status = supervisor.status(&row.id).await;
    let tools = if status.running {
        supervisor.tools(&row.id).await
    } else {
        Vec::new()
    };
    let unavailable = if status.running {
        None
    } else {
        match http_auth_config_error(&row).map_or_else(
            || McpTransportConfig::from_server_config(&row, DEFAULT_REQUEST_TIMEOUT),
            Err,
        ) {
            Ok(_) => dead_or_never_started(&row.id, &status),
            Err(error) => Some(McpServerUnavailable {
                code: McpFailureCode::of(&error),
                message: error.to_string(),
            }),
        }
    };
    McpServerView {
        config: row,
        status,
        tools,
        unavailable,
    }
}

/// 没在跑的**理由**：崩过（有退出记录）与从没启动过是两件不同的事，界面要分开说。
pub(super) fn dead_or_never_started(
    server_id: &str,
    status: &McpServerStatus,
) -> Option<McpServerUnavailable> {
    let exit = status.last_exit.clone()?;
    Some(McpServerUnavailable {
        code: McpFailureCode::Exited,
        message: describe_dead(server_id, Some(exit), status.restart.clone()),
    })
}

pub(super) fn load(database: &Database, server_id: &str) -> Result<McpServerConfig, String> {
    database.mcp_servers().get(server_id).map_err(|error| {
        if matches!(error, DatabaseError::NotFound) {
            format!("找不到 MCP 服务器 `{server_id}`；列表可能已经过期，刷新后重试。")
        } else {
            format!("读取 MCP 服务器 `{server_id}` 失败：{error}")
        }
    })
}

pub(super) fn describe_delete_failure(server_id: &str, error: &DatabaseError) -> String {
    if matches!(error, DatabaseError::NotFound) {
        format!(
            "删除 MCP 服务器 `{server_id}` 失败：它已经不在列表里了（可能是另一个窗口删掉了）。"
        )
    } else {
        format!("删除 MCP 服务器 `{server_id}` 失败：{error}")
    }
}

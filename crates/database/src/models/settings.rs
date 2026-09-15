//! 应用级设置。目前只有一项：出站代理（[ADR 0022](../../../../docs/adr.md)）。

use serde::{Deserialize, Serialize};
use yukinal_net::NetworkProxyMode;

/// 出站 HTTP 走直连还是系统代理。
///
/// 非机密：这里只有模式与一个凭据库引用。代理的用户名与密码进系统凭据库，不进 SQLite，
/// 也不进任何响应或日志 —— 与 client secret、HTTP 认证头同一条规则。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkProxyConfig {
    /// 默认直连：装上代理软件不该悄悄改变应用的连接路径。
    #[serde(default)]
    pub mode: NetworkProxyMode,
    /// 凭据库引用，值是 `user:password`；`None` 表示代理不需要认证。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
}

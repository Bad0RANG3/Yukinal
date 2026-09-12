//! add-server / update-server input（带 secret 的瞬时输入；secret 只进 keychain，不落 SQLite）。

use serde::Deserialize;

use super::server::Environment;

/// add-server 的认证输入（带 secret 的瞬时输入；secret 只进 keychain，不落 SQLite）。
///
/// `rename_all_fields = "camelCase"` **不是装饰**：枚举上的 `rename_all` 只改**变体名**，
/// 不变体里的字段名。少了它，`PrivateKey` 变体要的是 `private_key_pem`、`Identity`
/// 变体要的是 `identity_id`，而共享契约（`packages/shared/src/schemas/server.ts`）发出
/// 的一直是 `privateKeyPem` / `identityId` —— 于是「SSH 私钥」和「引用已有身份」两条路
/// 在 `server_add`/`server_update` 上永远只会得到 `missing field`，只有密码认证能通。
#[derive(Debug, Clone, Deserialize)]
#[serde(
    tag = "method",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AuthenticationInput {
    Password {
        password: String,
    },
    PrivateKey {
        private_key_pem: String,
        /// 加密私钥的口令。空 / 纯空白 = 「没有口令」，与 `crates/ssh` 的规则一致
        /// （`load_private_key` 也把空口令过滤掉），由调用点决定是否落 keychain。
        passphrase: Option<String>,
    },
    /// ssh-agent 认证：**不携带任何 secret**，也不写 keychain 条目。
    ///
    /// agent 持有的身份由远端 agent 自己保管，Yukinal 只转交签名请求；所以这个
    /// 变体没有字段 —— 一个只描述「用哪条路径发现 agent」的 socket path 属于连接
    /// 期决策（`Authentication::Agent { socket_path: None }` = 按平台约定发现），
    /// 不该在新增服务器时被固化进数据库。
    Agent,
    /// 引用已存在的身份（不改凭据）。
    Identity {
        identity_id: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddServerInput {
    pub name: String,
    pub host: String,
    pub port: Option<u16>,
    pub username: String,
    pub environment: Environment,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    pub authentication: AuthenticationInput,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateServerInput {
    pub server_id: String,
    pub name: String,
    pub host: String,
    pub port: Option<u16>,
    pub username: String,
    pub environment: Environment,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    pub authentication: Option<AuthenticationInput>,
}

impl UpdateServerInput {
    pub fn from_value(value: &serde_json::Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }
}

impl AddServerInput {
    /// 从跨层 JSON 反序列化（与 `@yukinal/shared` 的 AddServerInput 同形）。
    pub fn from_value(value: &serde_json::Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }
}

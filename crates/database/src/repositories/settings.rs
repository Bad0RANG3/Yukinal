//! `app_settings`：一行一个 JSON 值的应用级设置。
//!
//! 表的形状刻意是通用的（键/值/更新时间），而读写的是**有类型的**模型：加一项设置不用
//! 再开一张表，而读出来的东西仍然必须解得出结构，坏掉的行会被报出来而不是当成默认值。

use rusqlite::{params, OptionalExtension};

use super::decode::decode_error;
use crate::models::NetworkProxyConfig;
use crate::{Database, Result};

/// 出站代理设置在这张表里的键。
const NETWORK_PROXY_KEY: &str = "network.proxy";

pub struct AppSettingsRepository<'a> {
    db: &'a Database,
}

impl<'a> AppSettingsRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// 出站代理设置。没存过就是默认值（直连）—— 「没设置」与「设为直连」是同一件事。
    pub fn network_proxy(&self) -> Result<NetworkProxyConfig> {
        self.db.with(|connection| {
            let raw: Option<String> = connection
                .query_row(
                    "SELECT value FROM app_settings WHERE key = ?1",
                    params![NETWORK_PROXY_KEY],
                    |row| row.get(0),
                )
                .optional()?;
            match raw {
                Some(raw) => {
                    // 解不出来就是「这一行坏了」，不是「用默认值」：默认值会让一次数据损坏
                    // 悄悄变成「直连」，而那正是这件事最不该有的降级。
                    let config: NetworkProxyConfig =
                        serde_json::from_str(&raw).map_err(|error| decode_error(0, error))?;
                    Ok(config)
                }
                None => Ok(NetworkProxyConfig::default()),
            }
        })
    }

    pub fn save_network_proxy(&self, config: &NetworkProxyConfig) -> Result<()> {
        let value = serde_json::to_string(config)?;
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO app_settings (key, value, updated_at)
                 VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                 ON CONFLICT(key) DO UPDATE SET value = ?2,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
                params![NETWORK_PROXY_KEY, value],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::NetworkProxyConfig;
    use yukinal_net::NetworkProxyMode;

    #[test]
    fn a_setting_that_was_never_saved_reads_back_as_the_default() {
        let db = Database::in_memory().expect("in-memory database");
        assert_eq!(
            db.app_settings().network_proxy().expect("read"),
            NetworkProxyConfig::default()
        );
        assert_eq!(
            NetworkProxyConfig::default().mode,
            NetworkProxyMode::Direct,
            "the default must stay direct: no silent proxying"
        );
    }

    #[test]
    fn the_network_proxy_setting_round_trips() {
        let db = Database::in_memory().expect("in-memory database");
        let config = NetworkProxyConfig {
            mode: NetworkProxyMode::System,
            credential_ref: Some("keychain://mcp/proxy-credential".to_string()),
        };
        db.app_settings().save_network_proxy(&config).expect("save");
        assert_eq!(db.app_settings().network_proxy().expect("read"), config);
    }
}

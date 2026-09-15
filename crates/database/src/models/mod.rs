//! Row types mirroring `@yukinal/shared` (same names, same camelCase serialisation).
//!
//! These are the *wire shapes*: `serde_json::to_value(server_row)` produces exactly
//! what the IPC contract expects, so the command layer can hand rows to the UI
//! without a second translation. Enum string values are the dot-free literals from
//! the shared package; serde rejects anything else, so a typo fails at load time,
//! not at display time.
//!
//! The types are grouped by domain (`server` / `provider` / `chat` / `activity` /
//! `execution` / `collector` / `input`) and every item is re-exported here, so the
//! existing `yukinal_database::models::X` paths are unchanged.

/// Wire-string for enum columns without allocating JSON just to unquote it.
///
/// Also emits `ALL`, the complete variant list, so tests can assert the wire
/// contract over every variant without keeping a second copy of the list. That
/// second copy is what let `ActivityType` ship a serde rule that disagreed with
/// `as_str` for `FileChange` and `AgentAction`: the variant list lived in three
/// places and only two of them were checked.
///
/// **`from_db` is generated from the same literal list**, which is the third place
/// closed. It used to be written out by hand as a reverse `match` after every
/// invocation — ten tables restating the mapping declared three lines above them.
/// A test (`every_enum_agrees_across_serde_as_str_and_from_db`) did catch drift, but
/// a test is a check, not a construction: the bug was still *representable*, and the
/// fix for a wrong entry was to notice the failure and edit the second table. Now
/// the reverse direction cannot disagree with the forward one, because there is only
/// one list.
///
/// The match arm uses `$str` as the pattern, so the two directions are generated
/// from the same token. Adding a variant means adding one `Variant => "literal"` pair
/// and nothing else.
///
/// Defined here and invoked from each domain submodule below: a `macro_rules!` macro
/// is textually scoped, so the submodules declared after this point can see it.
macro_rules! enum_as_str {
    ($ty:ident, $($variant:ident => $str:literal),+ $(,)?) => {
        impl $ty {
            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            #[must_use]
            pub fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $str),+
                }
            }

            /// Inverse of [`Self::as_str`]: parse a value read back from SQLite.
            ///
            /// `None` for an unrecognised string, so a row written by a newer schema
            /// (or corrupted by hand) surfaces as a typed error at the call site
            /// rather than being silently coerced to a default variant.
            #[must_use]
            pub fn from_db(raw: &str) -> Option<Self> {
                match raw {
                    $($str => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

mod activity;
mod chat;
mod collector;
mod execution;
mod input;
mod provider;
mod server;
mod settings;

pub use activity::{Activity, ActivityOutcome, ActivitySource, ActivityType};
pub use chat::{ChatMessage, ChatMessageRole, ChatSession, ChatSessionCounts};
pub use collector::{CollectorSample, ContainerInfo, ServerSnapshot};
pub use execution::{PermissionMode, RiskLevel, ToolExecutionRecord, ToolExecutionStatus};
pub use input::{AddServerInput, AuthenticationInput, UpdateServerInput};
pub use provider::{
    AiProviderConfig, AiProviderKind, InfrastructureProviderConfig, McpHttpAuthHeaderConfig,
    McpOAuthClientAuth, McpOAuthConfig, McpOAuthFlow, McpServerConfig, ProviderModelOption,
};
pub use server::{
    Environment, HealthState, HostCertificateAuthority, Identity, Server, ServerCapabilities,
    ServerConnection, ServerGroup, ServerMetadata, ServerStatus, Workspace, WorkspaceRepository,
};
pub use settings::NetworkProxyConfig;

#[cfg(test)]
mod tests {
    use super::*;

    /// Serde, `as_str` and `from_db` are three encodings of one contract, and
    /// the IPC payload is produced by whichever one the call site happens to use.
    /// This asserts all three agree for *every* variant of every enum, which is
    /// the check that was missing when `ActivityType` serialised `FileChange` as
    /// `"filechange"` while the database and the shared TypeScript contract both
    /// said `"file_change"`.
    ///
    /// Driven by the same macro invocation that defines `as_str`, so a new
    /// variant cannot be added without being covered here.
    ///
    /// What each assertion still buys, now that `from_db` is generated from the
    /// same literal list as `as_str`:
    ///
    /// - the serde comparison remains an **independent** check. `serde`'s
    ///   `rename_all` rule is its own encoder; nothing generates it from the macro
    ///   list, so a multi-word variant can still drift — which is the original bug.
    /// - the `from_db` round-trip is no longer proof that the two directions agree
    ///   (the macro makes that structural). It is retained because it still catches
    ///   a **duplicate literal** in the invocation: two variants sharing a string
    ///   make the earlier pattern shadow the later one, so the shadowed variant does
    ///   not round-trip. Note this is now defence in depth rather than the sole
    ///   detector — a duplicated literal also breaks the serde comparison above,
    ///   since `rename_all` derives each variant's own spelling while `as_str` was
    ///   handed a shared one. Kept because it is one cheap assertion and states the
    ///   round-trip property directly.
    macro_rules! assert_wire_contract {
        ($ty:ident) => {
            for variant in $ty::ALL {
                let expected = variant.as_str();

                let serialised = serde_json::to_value(variant).unwrap_or_else(|error| {
                    panic!(
                        "{}::{} failed to serialise: {error}",
                        stringify!($ty),
                        expected
                    )
                });
                assert_eq!(
                    serialised,
                    serde_json::Value::String(expected.to_string()),
                    "{} serialises as {serialised} but as_str() says {expected:?}",
                    stringify!($ty),
                );

                assert_eq!(
                    $ty::from_db(expected),
                    Some(*variant),
                    "{}::from_db({expected:?}) does not round-trip",
                    stringify!($ty),
                );
            }
        };
    }

    #[test]
    fn every_enum_agrees_across_serde_as_str_and_from_db() {
        assert_wire_contract!(ServerStatus);
        assert_wire_contract!(Environment);
        assert_wire_contract!(HealthState);
        assert_wire_contract!(RiskLevel);
        assert_wire_contract!(PermissionMode);
        assert_wire_contract!(ActivityType);
        assert_wire_contract!(ActivitySource);
        assert_wire_contract!(ActivityOutcome);
        assert_wire_contract!(ChatMessageRole);
        assert_wire_contract!(ToolExecutionStatus);
        // `AiProviderKind` 是 `kebab-case`，不是上面那批 `lowercase`/`snake_case` 里的任何一个：
        // 它是唯一一个多词变体由连字符拼接的枚举，也正因为如此才需要这条断言。
        assert_wire_contract!(AiProviderKind);
    }

    /// The multi-word variants specifically: these are the ones a `lowercase`
    /// serde rule silently mangles, and the regression this guards against.
    #[test]
    fn multi_word_activity_types_use_snake_case() {
        assert_eq!(
            serde_json::to_value(ActivityType::FileChange).unwrap(),
            serde_json::json!("file_change"),
        );
        assert_eq!(
            serde_json::to_value(ActivityType::AgentAction).unwrap(),
            serde_json::json!("agent_action"),
        );
    }

    /// Single-word variants must not gain underscores from the `snake_case` rule.
    #[test]
    fn single_word_activity_types_stay_flat() {
        for variant in ActivityType::ALL {
            let wire = variant.as_str();
            if !wire.contains('_') {
                assert_eq!(
                    serde_json::to_value(variant).unwrap(),
                    serde_json::Value::String(wire.to_string()),
                );
            }
        }
    }

    fn add_server(authentication: serde_json::Value) -> Result<AddServerInput, String> {
        AddServerInput::from_value(&serde_json::json!({
            "name": "db",
            "host": "10.0.0.5",
            "username": "root",
            "environment": "staging",
            "authentication": authentication,
        }))
    }

    /// `authentication` 是 React ↔ Rust 的一处**联合类型契约**，而两端各自声明它
    /// （`packages/shared/src/schemas/server.ts` 的 `discriminatedUnion` 与这里的
    /// 内部标签枚举）。两边的拼写只能靠用例对上：TS 侧丢进来的就是下面这几个 JSON，
    /// 任何一个变体名或字段名漂了，命令层会在运行时才报「invalid add-server input」。
    ///
    /// 左边的形状就是共享 schema 产出的形状（见 `schemas/server.test.ts`）。
    #[test]
    fn authentication_input_accepts_the_shared_wire_shapes() {
        let parsed = add_server(serde_json::json!({ "method": "password", "password": "hunter2" }))
            .expect("password");
        assert!(matches!(
            parsed.authentication,
            AuthenticationInput::Password { password } if password == "hunter2"
        ));

        let parsed = add_server(serde_json::json!({
            "method": "privateKey",
            "privateKeyPem": "-----BEGIN OPENSSH PRIVATE KEY-----",
            "passphrase": "hunter2",
        }))
        .expect("encrypted private key");
        assert!(matches!(
            parsed.authentication,
            AuthenticationInput::PrivateKey { passphrase: Some(passphrase), .. } if passphrase == "hunter2"
        ));

        // 明文 key：`passphrase` **缺席**，不是空串。
        let parsed = add_server(serde_json::json!({
            "method": "privateKey",
            "privateKeyPem": "-----BEGIN OPENSSH PRIVATE KEY-----",
        }))
        .expect("plaintext private key");
        assert!(matches!(
            parsed.authentication,
            AuthenticationInput::PrivateKey {
                passphrase: None,
                ..
            }
        ));

        let parsed = add_server(serde_json::json!({
            "method": "certificate",
            "privateKeyPem": "-----BEGIN OPENSSH PRIVATE KEY-----",
            "certificatePath": "/home/dev/.ssh/id_ed25519-cert.pub",
            "privateKeyPath": "/home/dev/.ssh/id_ed25519",
        }))
        .expect("user certificate");
        assert!(matches!(
            parsed.authentication,
            AuthenticationInput::Certificate {
                certificate_path,
                private_key_path: Some(private_key_path),
                ..
            } if certificate_path.ends_with("-cert.pub") && private_key_path.ends_with("id_ed25519")
        ));

        // ssh-agent：一个字段都没有的变体 —— 这正是 `z.strictObject({ method })` 的形状。
        let parsed = add_server(serde_json::json!({ "method": "agent" })).expect("agent");
        assert!(matches!(parsed.authentication, AuthenticationInput::Agent));

        let parsed = add_server(serde_json::json!({ "method": "identity", "identityId": "idn_1" }))
            .expect("identity");
        assert!(matches!(
            parsed.authentication,
            AuthenticationInput::Identity { identity_id } if identity_id == "idn_1"
        ));

        // 未知 method 必须失败，而不是落到某个默认变体上。
        assert!(add_server(serde_json::json!({ "method": "kerberos" })).is_err());
    }

    /// `Identity` 的线形：`passphraseRef` 只在存在时出现，`agent` 身份的
    /// `credentialRef` 是空串（它没有凭据条目）。
    #[test]
    fn identity_serialises_the_passphrase_reference_only_when_present() {
        let encrypted = Identity {
            id: "idn_1".into(),
            label: "deploy key".into(),
            method: "privateKey".into(),
            credential_ref: "keychain://ssh/srv_1".into(),
            passphrase_ref: Some("keychain://ssh/srv_1-passphrase".into()),
            private_key_path: None,
            certificate_path: None,
            created_at: "2026-01-01T00:00:00.000Z".into(),
        };
        assert_eq!(
            serde_json::to_value(&encrypted).unwrap()["passphraseRef"],
            serde_json::json!("keychain://ssh/srv_1-passphrase"),
        );

        let agent = Identity {
            passphrase_ref: None,
            credential_ref: String::new(),
            method: "agent".into(),
            ..encrypted
        };
        let value = serde_json::to_value(&agent).unwrap();
        assert!(value.get("passphraseRef").is_none());
        assert_eq!(value["credentialRef"], serde_json::json!(""));
    }
}

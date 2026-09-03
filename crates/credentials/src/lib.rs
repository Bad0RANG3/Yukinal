//! yukinal-credentials — OS 凭据存储。
//!
//! 规则：
//! - 私钥、SSH 密码、API Key、Cloud Credential、Provider Token **绝不写入 SQLite**。
//! - SQLite 只保存引用：`credential_ref = "keychain://ssh/<id>"`。
//! - 后端：macOS Keychain / Windows Credential Manager / Linux Secret Service。
//!
//! 这是全项目唯一的 secret-sensitive 出入口（-R4），
//! 其他 crate / agent runtime 只能拿到 `credential_ref`，由本 crate 在使用时解析。
//!
//! 当前为契约占位。

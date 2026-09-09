# ADR 0002：SSH 后端使用 russh

Status: Accepted
Date: 2026-09-09

## Context

Yukinal 需要跨平台 SSH、远程命令、PTY 和 SFTP，同时希望连接能力可以在不改变 UI、Agent 或数据库的情况下替换。候选方案包括 russh、libssh2 绑定和调用系统 `ssh` 子进程。

## Decision

`crates/ssh` 使用 russh 作为默认实现，并通过 `SshBackend` trait 向其他 crate 暴露能力。russh 的类型不越过 `crates/ssh` 的抽象边界。

实现约束：

- 连接、认证、命令、PTY 和 SFTP 都沿 tokio 异步路径执行。
- 连接和命令有超时及取消语义；只对明确的传输错误做一次重连重试。
- 主机指纹保存在数据目录的 `known_hosts` 中。当前桌面命令使用 Trust On First Use：首次认证成功后记录指纹，已有指纹不匹配时拒绝连接。
- 当前支持密码和未加密私钥认证；SSH Agent 认证和带密码私钥暂未实现。

## Consequences

- 不需要依赖系统 SSH 客户端，三平台行为和 PTY/SFTP 路径更容易统一测试。
- russh 的 0.x API 变化被限制在 `crates/ssh` 内，调用方依赖稳定 trait。
- 某些老旧服务器的算法兼容性可能需要后续适配；上层不应为单一后端泄漏特判。
- Trust On First Use 适合开发和首次配置流程，但生产使用者必须在首次连接前核验主机指纹。

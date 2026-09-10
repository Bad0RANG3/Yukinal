# ADR 0002 SSH 后端采用 russh

Status: Accepted
Date: 2026-09-09

## Context

Yukinal 需要跨平台的 SSH 连接、远程命令、PTY 和 SFTP，并希望在不改变 UI、Agent 或数据库调用方的情况下保留替换底层实现的能力。候选方案包括 russh、libssh2 绑定和调用系统 `ssh` 子进程。

## Decision

`crates/ssh` 使用 russh 作为默认实现，并通过 `SshBackend` trait 向其他 crate 暴露 SSH 能力。russh 的类型不越过 `crates/ssh` 的抽象边界。

- 连接、认证、命令、PTY 和 SFTP 都在 tokio 异步路径上执行。
- 连接和命令支持超时与取消；仅对明确的传输错误进行一次重连重试。
- 主机指纹保存在数据目录的 `known_hosts`。桌面端采用 Trust On First Use：首次成功认证时记录指纹，后续不匹配即拒绝连接。
- 当前认证方式限于密码和未加密私钥；SSH Agent 和受密码保护的私钥尚未支持。

## Consequences

- 项目不依赖系统 SSH 客户端，三平台的 PTY、SFTP 和连接行为更便于统一测试。
- russh 的 0.x API 变化被封装在 `crates/ssh` 内，调用方依赖稳定的 trait。
- 老旧服务器的算法兼容性可能需要后续适配，但此类特判不应泄漏到上层模块。
- Trust On First Use 适合开发和首次配置流程；生产使用者必须在首次连接前独立核验主机指纹。

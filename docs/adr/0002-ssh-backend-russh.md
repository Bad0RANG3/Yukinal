# 0002 — SSH backend 使用 russh

Status: accepted (2026-09)

## Context
要求 SSH backend 可替换； 要求复用成熟开源实现；/ 要求三平台小体积。
候选：`russh`（纯 Rust / tokio）、`ssh2`（libssh2 C 绑定）、调用系统 `ssh`（子进程）。

## Decision
`crates/ssh` 默认实现基于 **russh**（workspace 锁 `0.63`），并保留 `SshBackend` trait 作为唯一出口。

理由：
1. 纯 Rust → 三平台交叉编译不需要 C toolchain 之外的东西，体积可控。
2. tokio 原生：与 PTY streaming / keepalive / 超时取消同一运行时。
3. client + channel + PTY + SFTP 能力覆盖 MVP 全清单。

## Consequences
- (−) russh 是 0.x，API 会变；因此 `SshBackend` trait 必须先写、russh 类型只能出现在 `crates/ssh` 内部。
- (−) 少数老服务器算法协商兼容性可能出问题 → 通过替换 backend（openssh-compat）解决，不在上层打补丁。
- (−) 需要审计依赖树：russh / russh-sftp / 其 crypto 依赖（ring vs aws-lc-rs）在实现时决定并记录。

# 未实现 TODO

> 这里只保留当前仍有实现或验证工作的事项。已完成、明确不做、已关闭的设计项和安全模型边界不再列出。
>
> 审计基线：2026-09-15 当前 Windows 工作树。校园网约束继续有效：只使用现有主机、本机进程、stdio 与回环地址；不启动 WSL、虚拟机、容器网络、桥接、热点，也不创建新的物理或虚拟网卡。
>
> 权威限制说明见 [`docs/limitations.md`](docs/limitations.md)。需要真实服务或目标平台的条目必须显式准备凭据和环境，不能用本地 fixture 冒充完成。

## P1：安全与网络边界

- [ ] **禁止 Markdown 图片在渲染时自动出站**
  - 默认只显示替代文字，或要求用户明确允许后才加载图片。
  - 若保留图片加载，必须使用来源白名单或受限代理，并限制请求目标、大小、超时和重定向。
  - 增加测试，证明不受信的模型、MCP 或工具输出不会悄悄触发 HTTP/HTTPS 请求。
  - 相关代码：`apps/desktop/src/lib/markdown/inline.ts`、`apps/desktop/src/components/MarkdownText.tsx`、`apps/desktop/src-tauri/tauri.conf.json`。

## P2：一致性、生命周期与供应链

- [ ] **绑定 HostKey 探针结果与实际端点**
  - 保存服务器配置期间禁用探针和信任操作。
  - 由服务端签发绑定 `serverId`、主机、端口、指纹、随机数和短 TTL 的探针票据；信任时校验票据，不能只接受调用方传入的指纹。
  - 覆盖“探针完成后修改主机/端口再信任”的竞态测试。

- [ ] **让 known_hosts 写入具备回滚和原子性**
  - `register` 保存失败时恢复内存状态。
  - 使用同目录临时文件、刷新和原子 rename，避免截断或部分写入留下损坏文件。
  - 增加写失败、崩溃恢复和并发写入测试。

- [ ] **统一 keychain、SQLite 和配置操作的失败语义**
  - Provider 保存失败时回滚已写入或已替换的凭据。
  - 服务器新增失败时清理已创建的身份、凭据和引用。
  - 删除后的凭据回收失败进入可重试的持久清理队列，不能留下无引用 secret。
  - 为每种失败点增加重试和重启后的 reconciliation 测试。

- [ ] **并行化 MCP 关闭流程并设置全局截止时间**
  - 各服务独立执行体面退出和强制终止。
  - 应用退出时间不随无响应服务数量线性增长，同时保留每个服务的关闭结果。

- [ ] **固定 CI Action 版本并补充 Rust 依赖审计**
  - 将 `checkout`、`setup-node`、`pnpm/action-setup`、Rust toolchain、cache 和 artifact action 固定到提交 SHA。
  - 在 CI 中启用可复现的 `cargo-audit` 或 `cargo-deny` advisory 检查。

- [ ] **同步公开文档与实际行为**
  - 修正 `docs/security.md` 对音频附件的过时描述。
  - 统一 `docs/boundaries/markdown.md` 对图片请求行为的说明。
  - 内部模块状态若继续保留，明确 `[ ]/[~]` 表示未完成的 DoD，而不是当前代码覆盖率。

## P3：正确性与内存硬化

- [ ] 给 `local_linux_only()` 增加真实的平台保护，或修正文档使其与实现一致。
- [ ] 对可能变成长查询、全表扫描或迁移的 SQLite 调用使用 `spawn_blocking`，并补充负载测试。
- [ ] 评估 `Secret` 的复制和内存清零策略，必要时引入 zeroize 并覆盖 keychain、sidecar 和错误路径。

## 需要外部环境的未验证项

- [ ] **真实 Anthropic/Gemini API 验证**
  - 在显式 live 环境变量和真实凭据下验证 streaming、tool call、取消、超时、429、5xx、malformed stream，以及图片、PDF、音频映射。
  - 记录模型、API 版本、日期和结果；不把付费请求放进默认 CI。

- [ ] **完整第三方 MCP 互操作矩阵**
  - 在现有抽样之外覆盖更多实现、认证方式、OAuth 发现变体、SSE/JSON/GET stream、取消和异常响应。
  - 所有网络测试必须显式 opt-in，并继续只使用现有网络设备。

- [ ] **跨平台打包与安装验收**
  - 在 macOS、Linux 和 Windows 目标环境实际构建、安装、启动并验证 `.app/.dmg`、`.deb/.rpm/.AppImage`、NSIS 和 WiX 包。
  - 当前机器无法替代目标平台，也不能通过新增网络设备规避该限制。

- [ ] **发布签名、公证和自动更新**
  - 准备发行凭据后配置 Windows/macOS 签名、公证和更新通道。
  - 在没有凭据和目标平台环境前保持未验证状态。

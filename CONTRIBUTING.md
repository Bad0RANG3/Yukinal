# 贡献指南

感谢你愿意参与 Yukinal。项目同时包含 Rust 宿主、Tauri 桌面端、Node.js Agent sidecar 和一套跨语言契约，因此最重要的贡献原则是：**先定义边界，再实现行为，最后用测试和文档把边界钉住。**

## 开始之前

1. 阅读 [文档索引](./docs/README.md)、[架构总览](./docs/architecture.md) 和[执行与授权模型](./docs/execution-model.md)。
2. 要改权限、目标解析、IPC、sidecar 协议或 MCP 时，先确认现有 ADR 和边界文档；改变已记录的决策应新增 ADR，而不是静默改写行为。
3. 不要在公开 Issue、提交、日志或测试 fixture 中放入真实密钥、令牌、私钥、主机凭据或机器专属路径。
4. 发现安全漏洞时不要开公开 Issue，按 [安全策略](./SECURITY.md) 私下报告。

## 开发环境

前置条件、系统依赖、启动方式和首次使用引导见 [开始开发](./docs/development.md)。最短路径：

```bash
pnpm install --frozen-lockfile
pnpm check
```

## 分支与提交

- 从最新 `main` 创建短生命周期分支；不要在共享分支上重写历史。
- 提交应保持单一目的，提交信息使用 Conventional Commits 风格，例如 `fix(ssh): ...`、`feat(agent): ...`、`docs: ...`。
- 不提交构建产物、编辑临时文件、真实凭据或与本提交无关的格式化改动。
- 变更跨层契约时，在同一个提交中更新类型、schema、生产者和消费者；无法原子更新时，先用测试暴露缺口。

## 修改规则

- Rust 拥有的进程、文件系统、SQLite、凭据与 SSH 形状不得下放到 WebView。
- Agent 只能提出工具调用；执行授权只由 Permission Engine 决策。不要通过提示词绕过权限规则。
- MCP 远端描述和 schema 始终是不可信数据，只可展示或交给模型，不可当作指令执行。
- 文档中的每项能力都必须能在实现或测试中找到依据；已知缺口写入 [当前限制](./docs/limitations.md)，补完后删除对应条目。

## 验证门禁

提交 PR 前至少运行：

```bash
pnpm check
git diff --check
```

`pnpm check` 已覆盖文档链接、密钥扫描、跨语言契约、类型检查、单元测试、sidecar 冒烟、打包契约、Rust 格式、Clippy 与 Rust 测试。修改打包或发布流程时还要按 [打包与分发](./docs/packaging.md) 验证安装包。

## Pull Request

PR 描述应回答：

- 改变了什么用户可见行为，为什么需要它？
- 触碰了哪些边界、契约或安全不变量？
- 如何验证？列出实际执行的命令与结果。
- 更新了哪些文档、ADR 或限制条目？
- 有哪些仍未验证的平台、网络或硬件条件？

审查通过以一扇绿色的 `pnpm check` 为下限；测试通过不替代对权限、数据边界和失败路径的审查。

## 许可证

提交贡献即表示你同意按项目的 [MIT License](./LICENSE) 发布原创代码与文档。第三方代码、字体或素材必须保留其许可证并在 [NOTICE](./NOTICE) 中登记。
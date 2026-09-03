# 0008 — 项目名 Yukinal

Status: accepted (2026-09)

## 词源

**Yukinal** 取自 **yugal**（西/葡语「轭」，拉丁 *iugum*，梵语 *yugá* —— 同根产生 yoga、conjugate、junction）。

轭的意象就是这款产品要干的事：把「人的意图」和「机器的执行力」套进同一副轭，同向用力。
它既不是"替你掌舵"（那会架空用户，而权限恰恰必须留在用户手里，见 ADR 0005），
也不是"更顺手的终端"（那会退化成工具集合）。
用户决定要去哪，agent 把力传到环境上，并在拉不动时回来报告。

## Context

曾用名在 npm / crates.io / PyPI / GitHub 四处都已被占用，且与开发者工具赛道上的既有产品同名，
搜索与命名空间都不可用。改名成本随时间单调上升：一旦发出安装包，`identifier` 会写进
Windows 安装身份与自动更新通道，届时再改就是一次迁移工程。所以**在第一个安装包之前定型**。

## Decision

项目名 **Yukinal**，并约定：

- 候选名的硬门槛是「同一时间窗内在 npm / crates.io / PyPI / GitHub org / 主流 TLD 上都可取得」，
  且不与目标用户熟悉的既有工具同名（这一条直接否决了 K8s 生态里的常见词）。
- Rust crate：`yukinal-ssh` / `yukinal-core` …（目录仍是 `crates/ssh`，路径依赖走 workspace）
- npm：`@yukinal/shared`、`@yukinal/agent-sdk`、`@yukinal/desktop`、`@yukinal/agent`
- 环境变量前缀：`YUKINAL_*`；协议常量 `YUKINAL_RPC_VERSION`；事件类型 `YukinalEvent`
- Tauri identifier：`dev.yukinal.workspace`
- 品牌书写永远用完整词 `Yukinal`，不出现相近前缀的缩写

## Consequences

- (+) 零同领域撞名，七字母三音节，CLI 可缩为 `yk`。
- (−) 生造词需要一次发音教育：读作 **YOU-ki-nal**，不是 *yugal*。
- (−) 仓库托管在个人账号下 `github.com/Bad0RANG3/yukinal`。同名 org 被一个无活动的占位账号持有，
  释放属于发布前事项，只影响安装脚本里的下载地址，不影响任何代码路径。
  规则：仓库内不要硬编码 `github.com/yukinal/...`，引用自身时用相对路径。
- (−) 现应用图标仍是「环 + 节点」的抽象母题，与「轭」的字面意象不完全贴合。
  换图不碰代码：改 `apps/desktop/design/app-icon.png` 后重跑
  `pnpm --filter @yukinal/desktop icon`。
- (−) 改名本身是一次全仓库机械改动（73 个文件），成本已付。

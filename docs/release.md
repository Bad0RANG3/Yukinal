# 交付与发布

## 2.0.3 验收范围（2026-10-28）

本版收录 2026-10-27 至 2026-10-28 的实机验收修复、持久任务/计划恢复、策略边界与桌面 UI 改动，并修复 Windows 任务栏图标。此前 `2.0.1` 的 Agent 验收结论已由后续证据修订；完整复现与逐项结果见[Agent 实机验收记录](./agent-live-acceptance-20261027.md)及[交接手册](./acceptance-handoff.md)。

- 确认并修复了 4 项缺陷（阶段把只读命令报成变更、拒绝后审计缺终态行、健康检查
  被迫走 destructive 审批、批准等待时间混入工具耗时），另修 2 项（偏好迁移
  静默放宽审批默认值、死代码）；
- 修复 1 项端到端能力缺失：桌面端重启后挂起的审批不再丢失——同窗口重发会交回同一个
  待批准调用，不同窗口重新开始；
- 修复 1 项设计意图缺口（拒绝后模型拿不到失败结果），加固 1 项提示词约束。

完整证据、复现步骤与修复说明见[Agent 实机验收记录（2026-10-27）](./agent-live-acceptance-20261027.md)。Computer Use 复验覆盖真实只读工具循环、一次 `server.info` 计划步骤成功结算及桌面重启后恢复；两份待选择任务和计划也在重启后恢复。独立 `server.exec` 审批测试保持自动执行关闭、不批准请求，等待 120 秒后显示审批过期，且发起时尚未执行。host denylist 与桌面 SSH autoconfig 联合通过实机验证：只读请求成功，命中策略的命令在 SSH 派发前被拒绝。结构化回复组件、复制、历史回开与任务绑定修复由桌面测试和 Computer Use 覆盖。

完整 `pnpm check` 通过：CTest 17/17、control plane 157 项、Python Agent 345 项、桌面 641 项；生产 UI bundle、Python sidecar smoke、文档链接、发布卫生与秘密扫描通过。Python 3.14 测试出现非致命资源警告。当前仍未覆盖真实多目标并行/长时间压力与安装版 GUI；对应本地策略、并行会话和长工作流回归通过。

Windows 任务栏图标修复将 Yukinal ICO 资源嵌入 C++ 宿主可执行文件，主窗口和独立工作页窗口均显式设置小/大图标，并设置稳定的 Windows AppUserModelID；安装包快捷方式与宿主共用同一图标源。

## 当前发行：2.0.3（2026-10-28）

`v2.0.3` 是本次修复与验收更新的正式版。`main` 保存源码，稳定标签触发三平台完整门禁、安装器 smoke 与正式 Release 发布；只有工作流全部成功后，GitHub Release 才包含本版安装包。

- [本版发行说明](./releases/2.0.3.md)、[变更记录](./changelog.md)
- [发行工作流与运行记录](https://github.com/Bad0RANG3/Yukinal/actions/workflows/release-candidate.yml)
- [本版 GitHub Release](https://github.com/Bad0RANG3/Yukinal/releases/tag/v2.0.3)
- [2026-10-20 及更早的发布记录归档](./release-history-20261020.md)

## 本版交互

文件、终端、日志、服务四按钮直接切换中央主界面，每次只显示一个工具视图。移除额外标签栏、新增标签菜单、分栏、展开和返回对话操作；Agent 对话在右侧浮栏继续。工具切换保留终端、目录、筛选与草稿，切换服务器后当前工具跟随目标并保留旧终端。详见[工作区交互说明](./workspace-tabs-spec.md)。本轮 UI 修订未改变后端执行和审批逻辑。

## 发布验收

| 范围 | 结论与来源 |
| --- | --- |
| Agent 能力 | **部分通过**：真实只读工具循环、持久计划结算和重启恢复、审批过期、host denylist 与桌面 autoconfig 联合工作、原生 UI 七项清单均已由 Computer Use/本地回归覆盖。真实多目标并行/长时压力与当前源码安装版 GUI 尚未验收，见[实机验收记录](./agent-live-acceptance-20261027.md) |
| UI 自动回归 | `pnpm check`：桌面 641 项通过；类型检查与生产构建通过 |
| Windows 任务栏图标 | Windows C++ 宿主编译嵌入 Yukinal ICO，主窗口和独立工作页窗口均设置图标；正式 installer smoke 由 v2.0.3 发布工作流验证 |
| 浏览器交互 | 模拟数据下检查 1381×1244、1024×768、390×844；连续切换四工具、跨服务器终端恢复、目录/筛选/草稿保留、参考面板恢复、弹层边界、Escape/外部关闭及焦点恢复；控制台无错误 |
| 模拟用量 | 仅开发 fixture 的 `diagnostics=tokens` 或 `diagnostics=1`，不代表真实 Provider 消耗 |
| 发行整理复核 | 更新依赖后，workspace 类型检查、共享库构建、桌面生产构建、文档链接、发布卫生与秘密扫描通过；`pnpm audit` 未发现已知漏洞 |
| 三平台本版安装包 | 以 `v2.0.3` 对应成功 workflow 的完整门禁、冻结 Agent、安装包构建和安装/卸载 smoke 为准 |
| 签名/公证 | 安装包未签名，macOS 应用未公证 |
| 其他环境组合 | 本次确认针对 Agent 能力；未新增逐端点、三平台 GUI 拖放或升级组合的公开验收记录 |

真实 Provider/SSH 证据限定为文首记录的只读调用、一个已结算且桌面重开后仍成功的计划步骤、一次未批准审批过期、denylist 拒绝与任务/计划恢复；没有把这些证据扩展为真实多目标长时压力、安装包 GUI 或所有端点组合通过。

## 版本与载荷

应用版本源是 `packages/shared/src/version.ts` 的 `APP_VERSION`。根清单、各 JavaScript workspace、`core_ping` fixture、原生宿主报告版本与 README 一起对齐。Python Agent 版本源为 `agent/pyproject.toml`，独立保持 `0.1.0`。

安装包包含 React 生产 UI、C++ WebView 宿主、PyInstaller 冻结 Python Agent 与 C++ SSH 服务，终端用户无需安装 Node.js/Python。平台 WebView 与动态库要求由安装环境决定。

`scripts/package.mjs` 暂存到 `target/release/bundle/yukinal-<version>-<platform>-<arch>/`，写出 `release-manifest.json` 和 `SHA256SUMS.txt`，记录源码修订、脏工作区、文件哈希/大小及可执行输入来源与构建时间。缺失宿主/UI/Agent/SSH 服务、缺少 UI 的 `index.html` 或输入陈旧时默认拒绝。`--allow-stale-inputs` 只供诊断，豁免会写入清单，不用于正式发行。

## 开发和本地验证

依赖与启动步骤见 [README](../README.zh-CN.md#构建与运行)。完整检查入口：

```bash
pnpm install --frozen-lockfile
pnpm check
pnpm --filter @yukinal/desktop dev -- --host 127.0.0.1
```

`pnpm check` 包含文档链接、发布卫生、秘密扫描、原生构建/CTest、Python Agent、控制平面、workspace 类型/测试及 sidecar smoke。本机环境降级不算三平台全量通过。

UI 可分别运行 `pnpm --filter @yukinal/desktop typecheck`、`test`、`build`。开发预览 `?fixture=1&diagnostics=tokens` 可复查用量弹层，`diagnostics=1` 同时包含上下文压力样例。

## 正式发布流程

1. 提交功能与文档，同步应用版本来源，在 `docs/releases/<version>.md` 写发行说明。
2. 推送 `main` 并核对对应源码检查；创建、推送新的稳定标签 `v<version>`，不移动已公开标签。
3. [release-candidate workflow](../.github/workflows/release-candidate.yml) 在 Windows、Ubuntu 24.04、macOS 15 Intel 运行完整门禁，冻结 Agent，构建 NSIS/DEB/DMG，并检查安装或复制启动、卸载与用户数据保留。
4. 三平台均成功后，publish job 校验标签和清单版本，读取对应版本发行说明，上传三个安装包、平台清单、载荷校验和及总 `SHA256SUMS.txt`，创建正式 GitHub Release 并设为 latest。
5. 核对本版源码、资产和校验和。失败时保留记录并修复原因，不以旧版或候选包宣称本版已交付。

手动 `workflow_dispatch` 只生成候选工件；带连字符的预览/RC 标签也不执行稳定 publish。候选工件保留 30 天，正式安装包以 Release 资产为准。

本地完成原生宿主、SSH 服务与 UI 构建后，可运行 `pnpm build:python-agent`、`pnpm package`、`pnpm package -- --verify` 暂存、复核载荷。通过 `--host-exe`、`--ssh-service-exe`、`--ui-dir` 指定输入，完整参数见 `node scripts/package.mjs --help`。本地产物不等于正式发行完成。

## 后续工作

后续工作包括真实多目标并行/长时间压力、安装版 GUI、签名、公证和平台升级覆盖。新增验收记录写清版本、环境、动作、结果和脱敏证据，不将本次确认延伸为所有外部组合或未来版本已通过。

真实 Provider 可用[显式启用的探针 workflow](../.github/workflows/provider-live-acceptance.yml)复核，它只验证声明的工具调用路径。凭据、私钥和完整模型上下文不写入发布文档。

# Yukinal 文档

当前发行版本为 `2.1.0`。本版打通了计划结算链路：Agent 可记录证据、结论与决策摘要，可跨 run 结算证据步、决策步与验证步，发布方案后停在等待用户而不是判失败；五个免审批固定读改走独立的 `read_probe` 结构化通道。用真实服务器与真实 Provider 跑过的一次完整流程（证据 → 计划 → 方案 → 批准 → 执行 → 验证）走到计划完成、任务完成。真实多目标并行和长时间压力、安装版 GUI、签名与公证仍未覆盖。详细状态见[2.1.0 发行说明](./releases/2.1.0.md)与[交付与发布](./release.md)。界面采用四按钮切换主工具、Agent 浮栏继续对话；主导航只有运维台与设置，运维台把任务看板、值守计划、动态流与模型摘要收成同一页的四个视图。

## 从哪里开始

| 目的 | 阅读 |
| --- | --- |
| 了解和启动项目 | [中文 README](../README.zh-CN.md)、[English README](../README.md) |
| 本版变化与安装包状态 | [2.1.0 发行说明](./releases/2.1.0.md)、[交付与发布](./release.md) |
| 四工具、浮栏、状态和键盘行为 | [工作区交互说明](./workspace-tabs-spec.md) |
| 布局与控件 | [界面表达与皮肤约定](./ui-design-system.md) |
| 用户路径与功能范围 | [产品定义](./product.md) |
| Agent 权限与方法 | [安全与数据边界](./security.md)、[运维 Agent 处理流程](./ops-agent-procedure.md) |
| RAG 知识索引与离线 CLI | [RAG 知识索引](./rag-knowledge-index.md)、[示例语料与评估](../examples/rag/README.md) |
| 终端输出着色 | [界面表达与皮肤约定](./ui-design-system.md) 的 content hue 一节 |

## 实现与历史交接

发行状态以[交付与发布](./release.md)为准，历史记录查 Git 历史。

| 主题 | 阅读 |
| --- | --- |
| C++ 宿主与构建 | [原生桌面宿主](./native-desktop-host.md)、[架构与构建计划](./architecture-and-build-plan.md) |
| SSH 加固 | [SSH 加固 2026-11](./ssh-hardening-2026-11.md) |
| 变更历史 | [变更记录](./changelog.md) |
| 审查结论与修复 | [审查总报告](./review-report.md)、[审查修复](./review-fixes.md) |

## 文档规则

- 当前说明与代码一致；旧设计在 Git 历史或标明日期的归档中保留。
- 负责人验收、自动测试、模拟 UI 和真实平台记录分别标注，不互相冒充证据。
- 新版本同步应用版本、README、变更记录和 `releases/<version>.md`。
- 凭据、审批、远端副作用变更同步[安全与数据边界](./security.md)，秘密不入文档。
- 平台或外部端点没有独立证据时，不扩大既有通过结论的范围。

贡献方式见[贡献指南](../CONTRIBUTING.md)。

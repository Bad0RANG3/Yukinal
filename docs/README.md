# Yukinal 文档

当前版本 `2.0.1`（已发行）。**Agent 能力验收已重开**：同日实机验收发现并修复 4 项缺陷，同时确认挂起审批在桌面端重启后无法恢复，见[Agent 实机验收记录](./agent-live-acceptance-20261027.md)。界面采用四按钮切换主工具、Agent 浮栏继续对话。

## 从哪里开始

| 目的 | 阅读 |
| --- | --- |
| 了解和启动项目 | [中文 README](../README.zh-CN.md)、[English README](../README.md) |
| 本版变化与安装包状态 | [2.0.1 发行说明](./releases/2.0.1.md)、[交付与发布](./release.md) |
| 四工具、浮栏、状态和键盘行为 | [工作区交互说明](./workspace-tabs-spec.md) |
| 布局与控件 | [桌面窗口与工作区](./desktop-window-direction.md)、[界面表达与皮肤约定](./ui-design-system.md) |
| 用户路径与功能范围 | [产品定义](./product.md) |
| Agent 权限与方法 | [安全与数据边界](./security.md)、[运维 Agent 处理流程](./ops-agent-procedure.md) |
| Agent 实机验收与缺陷 | [实机验收记录 2026-10-27](./agent-live-acceptance-20261027.md) |
| 长任务、推理和上下文 | [AI 运行与对话体验](./ai-runtime-performance.md) |
| 图形与检测扩展 | [AI 绘图协议](./ai-visual-protocol.md)、[服务器检测工具包](./server-toolkit.md) |
| 审计与版本 | [第五轮审计](./audit-round5.md)、[变更记录](./changelog.md) |
| 终端输出着色 | [界面表达与皮肤约定](./ui-design-system.md) 的 content hue 一节 |

## 实现与历史交接

旧日期的“当前”记录按其时间和源码理解；发行状态以[交付与发布](./release.md)为准。

| 主题 | 阅读 |
| --- | --- |
| C++ 宿主与重写 | [原生宿主](./native-desktop-host.md)、[重写方案](./rewrite-cpp-python.md) |
| 施工与命令清单 | [目标架构](./architecture-and-build-plan.md)、[剩余目标交接](./handover.md)、[桌面施工交接](./cpp-desktop-handoff.md) |
| 迁移历史 | [C++ 接管与 Rust 移除](./rust-removal-plan.md) |
| 早期验收 | [SSH + Agent 验收交接](./acceptance-handoff.md)、[旧发布记录](./release-history-20261020.md) |

## 文档规则

- 当前说明与代码一致；旧设计在 Git 历史或标明日期的归档中保留。
- 负责人验收、自动测试、模拟 UI 和真实平台记录分别标注，不互相冒充证据。
- 新版本同步应用版本、README、变更记录和 `releases/<version>.md`。
- 凭据、审批、远端副作用变更同步[安全与数据边界](./security.md)，秘密不入文档。
- 平台或外部端点没有独立证据时，不扩大既有通过结论的范围。

贡献方式见[贡献指南](../CONTRIBUTING.md)。

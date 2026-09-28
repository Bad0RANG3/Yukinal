# 文档索引

这份索引是文档入口。阅读当前行为时，以代码、测试和下列**现行文档**为准；[历史记录](./history/README.md)保留审计时的上下文，不继续充当待办清单。根目录的 [README](../README.md)只介绍产品、能力和启动方式。

## 从哪里开始

| 目的 | 入口 |
| --- | --- |
| 了解现在做得到什么 | [产品介绍](../README.md)、[当前限制](./limitations.md) |
| 选择下一步实现什么 | [下一阶段实施计划](./implementation-plan.md) |
| 理解运行链路和资源归属 | [架构总览](./architecture.md)、[执行与授权模型](./execution-model.md) |
| 改权限或安全策略 | [安全与数据边界](./security.md)、[read](./risk-tiers/read.md)、[write](./risk-tiers/write.md)、[dangerous](./risk-tiers/dangerous.md)、[ADR](./adr.md) |
| 改外部协议或内容渲染 | [Provider](./boundaries/provider.md)、[MCP](./boundaries/mcp.md)、[Markdown](./boundaries/markdown.md) |
| 开发与交付 | [开始开发](./development.md)、[外部验证运行手册](./external-validation.md)、[打包与分发](./packaging.md)、[版本与发布历史](./changelog.md) |
| 复核旧审计与已完成工作包 | [历史记录](./history/README.md) |

## 目录职责

| 位置 | 内容与更新方式 |
| --- | --- |
| `docs/*.md` | 当前跨模块契约、限制、实施路线、开发与发布事实；行为变化时同步维护 |
| `docs/boundaries/` | Provider、MCP、Markdown 三个跨模块接口 |
| `docs/risk-tiers/` | 三档权限及各自的批准规则 |
| `docs/history/` | 有日期和基线的审计、重构记录、已完成旧计划；只修坏链接或明显的归档说明，不在里面追加现行规则 |
| `docs/assets/` | 文档图片等资产，随引用它的文档维护 |

## 单一事实来源

- **当前能力**写在根 [README](../README.md) 和对应边界文档；**当前做不到的事及有意保留的边界**写在[当前限制](./limitations.md)；**准备怎么做**写在[下一阶段实施计划](./implementation-plan.md)；**已做完的变化**写在[版本与发布历史](./changelog.md)。不要把旧审计里的将来时误读为当前待办。
- 架构和授权理由以 [ADR](./adr.md) 为准。改变已有决定时新增一条并标明取代关系，说明决定、理由、代价；不要悄悄改写旧决定。
- 具体函数与常量的解释留在代码旁。文档只保留跨模块契约、安全模型、用户可见边界和无法从一份源码理解的取舍。

## 修改时检查

1. 功能范围变化时同步更新根 [README](../README.md)、对应边界文档、[当前限制](./limitations.md)与[版本与发布历史](./changelog.md)。缺口解决后从限制列表移除；计划完成后更新实施状态。
2. 声称“支持”或“已验证”时给出代码、测试或真实运行记录。fixture、回环和真实端点的结论分别写清。
3. 相对链接和标题锚点运行 `node scripts/check-docs-links.mjs`；发布用词运行 `node scripts/check-publication.mjs`。完整门禁为 `pnpm check`。
4. 新文档先判断能否并入现有主题；历史快照放 `history/`，不要和现行规范并排。

仓库协作与报告入口仍在根目录：[贡献指南](../CONTRIBUTING.md)、[安全策略](../SECURITY.md)、[行为准则](../CODE_OF_CONDUCT.md)。

# 审查报告索引（三轮）

本目录是 2026-10-18 对 Yukinal（`D:\yukinal`）做**三轮系统性审查 + 优化修复**的原始报告。
全部报告**未被跟踪**（`git status` 显示 `?? docs/review/`），是审查过程证据，不是仓库文档；
对外交付的汇总结论请见 [`docs/review-report.md`](../review-report.md) 与 [`docs/review-fixes.md`](../review-fixes.md)。

## 基线

- `HEAD = 736f06a58d07e595fa79ac620d8ce116779bfe4c`（分支 `codex/ui-layout-direction`）
- 平台 Windows 11 / MSVC VS2022 Community 14.44.35207 / CMake `NMake Makefiles`
- 审查期间工作区多有并行修复，各报告**自带「读取版本」声明**；凡涉及「门禁覆盖」的结论都以报告声明的哈希为准。

## 第一轮：广度审查

| 报告 | 范围 | 计数 |
| --- | --- | --- |
| [`r1-native.md`](r1-native.md) | `native/src`、`native/include/yukinal`、`native/tests`、CMake | Critical 0 / High 2 / Medium 3 / Low 4 |
| [`r1-desktop.md`](r1-desktop.md) | `native/desktop_host`、`apps/desktop`、`packages/shared|agent-sdk|provider-sdk` | Critical 2 / High 4 / Medium 5 / Low 3 |
| [`r1-python.md`](r1-python.md) | `agent`、`services/control-plane`、`scripts`、CI 与配置 | Critical 0 / High 3 / Medium 5 / Low 5 |

## 第二轮：深度审查（按子系统）

| 报告 | 焦点 | 计数 |
| --- | --- | --- |
| [`r2-release.md`](r2-release.md) | 构建 / 打包 / 发布链路、`verify` 完整性边界、CI 差异 | Critical 0 / High 3 / Medium 7 / Low 4 |
| [`r2-control-plane.md`](r2-control-plane.md) | 控制平面多租户与节点签名（唯一多租户组件） | 7 项威胁判定 + F-1..F-10 + 83 个可复现探针（编号延伸至 P505）+ 修复记录第七节 |
| [`r2-approval.md`](r2-approval.md) | 审批 / 授权状态机（host ledger ↔ sidecar admission） | Critical 0 / High 2 / Medium 3 / Low 2 |
| [`r2-contract-matrix.md`](r2-contract-matrix.md) | 全部 86 条 IPC 命令 × 宿主 / sidecar / UI 三方穷举 | 端到端可达 81 / 死命令 3 / 仅 fixture 2 |
| [`r2-agent-lifecycle.md`](r2-agent-lifecycle.md) | Agent 运行生命周期、取消传播、并发、resume、审计 | High 4 / Medium 4（其中 4 项是永久挂起） |

## 第三轮：对抗式证伪

| 报告 | 焦点 |
| --- | --- |
| [`r3-verification.md`](r3-verification.md) | 对 R1 全部 11 条 Critical/High **默认假定为假**重新核验：成立 6 / 部分成立 3 / 原报告成立但当前已修复 2 / 不成立 0 |

R3 推翻或修正了 R1 的四处断言，这些修正**已并入**最终交付的两份汇总文档：

1. `native` H-1「`exit_code < 0` 守卫是死代码」→ **推翻**（守卫可达，但语义错误）。
2. `desktop` H-3「未知 approvalId 被当成成功 / 可越权」→ **推翻越权**，降级为「静默 no-op + 误导性回执」。
3. `desktop` H-2「sidecar 多一个字段就把 UI 打崩」→ **推翻现状性**，真实风险是「将来加字段即崩」。
4. `desktop` C-1 的定性 → 修正为**宿主接线缺口**（服务层已实现且有测试）。

## 阅读顺序建议

1. 先读 [`docs/review-report.md`](../review-report.md) 与 [`docs/review-fixes.md`](../review-fixes.md) 拿结论；
2. 需要细节再按上表进入对应报告；
3. 对任何单条结论存疑时，先读 [`r3-verification.md`](r3-verification.md) —— 它给出了每条争议项的 file:line 正证据与最小复现命令。

## 这些报告**不能**作为「已修复」的证据

R1 / R2 的前六节记录的是**修复前**状态，刻意未追改（以保留原始证据）。要看「修了什么、怎么验证的」，
请读修复任务自己的验收记录与 [`docs/review-fixes.md`](../review-fixes.md)。

R1 报告尤其要注意时效：它的结论在修复过程中被改写过，引用前请先看下表的当前口径，不要直接引用 R1 正文的定性。

| R1 原结论 | 当前口径 | 依据 |
| --- | --- | --- |
| `native` H-1「`exit_code < 0` 守卫是死代码」 | **不成立**：守卫可达但语义错误（「未收到 exit-status」与「退出码 0」不可区分 → 假成功；合法大退出码被误报失败 → 假失败） | [`r3-verification.md`](r3-verification.md)；修复与差异回归见 `review-fixes.md` 的 FIX-8 |
| `native` H-2「Linux 上 RST 可终止进程」 | **成立**，但本轮只交付**源码级护栏**（本机无 Linux，行为级未验证） | `review-fixes.md` 的 FIX-8 与 FIX-16 |
| `desktop` H-3「未知 `approvalId` 被当成成功 / 可越权」 | **越权不成立**，降级为「静默 no-op + 误导性回执」；回执部分已修 | [`r3-verification.md`](r3-verification.md)；FIX-9 |
| `desktop` H-2「sidecar 多一个字段就把 UI 打崩」 | **现状性不成立**（当前 0 条会抛），真实风险是「将来加字段即崩」；已按「容错 + 漂移可观测」修掉 | [`r3-verification.md`](r3-verification.md)；FIX-10 |
| `desktop` C-1「keyboard-interactive 不可用」 | 定性**修正为宿主接线缺口**（服务层已实现且有测试），不是服务层缺陷 | [`r3-verification.md`](r3-verification.md)；FIX-11 |
| `python` H-1 / H-2（门禁覆盖落差） | 成立且**已修**（Agent 全量入闸、控制平面不再静默跳过） | FIX-3 |
| 各报告正文里的 Medium / Low | 多数**仍未修**，按原样保留 | `review-fixes.md` 的「未修项」一节 |

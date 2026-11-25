# 审查报告索引（四轮）

本目录是 2026-10-18 起对 Yukinal（`D:\yukinal`）做系统性审查 + 优化修复的原始报告。
第一至第三轮（r1/r2/r3）的报告是审查过程证据；第四轮（r4）是面向 Agent 工程能力的**专门审计**，
四轮报告**均已纳入版本控制**（`git ls-files docs/review` 可查）。
对外交付的汇总结论请见 [`docs/review-report.md`](../review-report.md) 与 [`docs/review-fixes.md`](../review-fixes.md)。

## 基线

- R1–R3：`HEAD = 736f06a58d07e595fa79ac620d8ce116779bfe4c`（分支 `codex/ui-layout-direction`）
- R4：`HEAD = f630442`（`Add curated knowledge RAG, memory library retrieval and v4 evaluation`），工作树干净
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

## 第四轮：Agent 工程能力专项审计

| 报告 | 焦点 | 计数 |
| --- | --- | --- |
| [`r4-long-run-memory-checkpoint.md`](r4-long-run-memory-checkpoint.md) | 长任务（goal loop / 预算）、记忆（显式记忆 + curated RAG）、检查点（持久化 / 恢复 / 审批跨重启），以及这三块对外文档声明的真实性 | 成立 22（Critical 5 / High 9 / Medium 8） |

R4 的取证方式与前三轮不同：不放信任何文档声明，每条结论都要么给出源码链，要么给出可复现探针输出。
它的主要发现是**主路径未接通**而不是「功能缺失」：`turn:<digest>` 复用键从不写入、严格 msgpack 从未生效、
记忆检索在 35 个汉字以上必然静默关闭、检查点明文落盘且 Windows 无权限隔离。
同时它给出一份对照表，纠正 `docs/security.md`、`agent/README.md`、
`docs/agent-live-acceptance-20261027.md` 三处与代码不符的对外声明。

### R4-FIX：同日的修复处置

R4 收尾后紧接着做了修复，处置记录写在报告**第七节**（第一节判定表保留审计当时的结论，未追改历史）。摘要：

- **已修 11 条**：C-1（显式注入 `JsonPlusSerializer(allowed_msgpack_modules=None)`）、C-2（`_prepare_resume_key` 直接写 `turn:<digest>`）、
  C-3（`_checkpoint_matches` 只绑 system prompt 与本轮 user message）、C-4（`project_query` 按 64-token 预算投影 + `memoryRetrieval` 可观测）、
  C-5（Windows 当前用户独占 DACL，删掉 `os.name != "nt"` 跳过分支）、H-4（复核解析失败一次有界修复重试 + 区分格式/证据错误码）、
  H-6（根因是 `CompiledStateGraph` 没有 `adelete_thread`；改为从 saver 取，丢弃成功后复用同一 thread id）、
  H-7（`_aborted_result` + effect 计数 + `outcome_unknown`，全 abort 有日志）、M-3/M-4（`context.memoryRetrieval` 契约 + UI 文案）、M-5（`_budget_binding` 剔除推断值）。
- **量化后判定不修 1 条**：M-1 —— 去掉 `coverage > 0` 门控只能多召回 1.5% 的哈希碰撞噪声（80 000 对实测），语义召回需换 embedding 模型。
- **既定设计、留证不改 2 条**：H-5（跨重启续跑靠「重发 + 重新批准」，已实证恰好执行一次）、M-7（有历史即关闭自动批准）。
- **路线图 7 条**：H-1、H-2、H-8、H-9、M-2、M-6、M-8。H-3 补了事实口径修正（`maxAttempts` 在宿主侧限制 investigation 重跑次数）。
- 修复期间新增两个回归文件：`agent/tests/test_checkpoint_reuse.py`（13 例）与 `agent/tests/test_checkpoint_write_failure.py`（5 例）；
  `agent` 全量 **504 tests OK (skipped=5)**，`@yukinal/shared` **151 tests pass**，`pnpm check` 见报告第七节。

## 阅读顺序建议

1. 先读 [`docs/review-report.md`](../review-report.md) 与 [`docs/review-fixes.md`](../review-fixes.md) 拿结论；
2. 需要细节再按上表进入对应报告；
3. 对任何单条结论存疑时，先读 [`r3-verification.md`](r3-verification.md) —— 它给出了每条争议项的 file:line 正证据与最小复现命令；
4. 关注 Agent 运行、记忆与恢复能力时，读 [`r4-long-run-memory-checkpoint.md`](r4-long-run-memory-checkpoint.md) —— 它是当前唯一覆盖「长任务 / 记忆 / 检查点」三块的实证报告。

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

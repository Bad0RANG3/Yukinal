# Yukinal 三轮审查总报告（2026-10-18）

本报告是 **R1 广度 → R2 深度 → R3 对抗式证伪** 三轮审查的汇总与交叉验证结论。原始证据保留在
[`docs/review/`](./review/README.md)（10 份报告、约 730 KB、全部未跟踪）。配套的修复说明见
[`docs/review-fixes.md`](./review-fixes.md)。

## 目录

1. [结论摘要](#1-结论摘要)
2. [审查基线与方法](#2-审查基线与方法)
3. [三轮审查的分工与覆盖](#3-三轮审查的分工与覆盖)
4. [严重度汇总表](#4-严重度汇总表)
5. [最值得优先修的 8 条](#5-最值得优先修的-8-条)
6. [R3 推翻或修正的断言](#6-r3-推翻或修正的断言)
7. [已修复清单](#7-已修复清单)
8. [已知未修 / 明确不做](#8-已知未修-明确不做)
9. [无法判定与需要外部环境的部分](#9-无法判定与需要外部环境的部分)
10. [第四轮（2026-10-19）](#10-第四轮2026-10-19)
11. [可复核性](#11-可复核性)

---

## 1. 结论摘要

**这个项目的单点实现质量明显高于同类工程。** 三轮审查（含一轮专门做证伪）没有找到任何
「本地无需前置条件即可造成凭据外泄 / 任意代码执行 / 越权」的路径。host 密钥信任是
「三选一且必须匹配」的 fail-closed 设计；审批票把 12 个绑定字段全部纳入比对，改一个就烧票，
且有逐个字段的测试；控制平面的节点签名、任务状态机、SQL 全部走绑定参数；C++ 宿主在
IPC 输入校验这一层做了 `only_fields` 白名单、`valid_*` 边界检查、凭据 scrubber 与迁移前
`VACUUM INTO` 备份。

**真正的风险不在单点实现，而在三条系统性断层：**

| # | 断层 | 典型表现 |
| --- | --- | --- |
| 1 | **门禁与实际执行之间存在落差** | Agent 测试只有 100/153 被 CTest 覆盖；控制平面 129 个测试在 CI 中从不运行；构建目录路径写错会让 CTest 步骤**静默跳过并报绿** |
| 2 | **跨语言契约只有一侧活着** | 19 个可订阅通道里 6 个在 C++/Python 里没有任何生产者；`agent_status.restart` 有契约、有 UI、无实现；测试断言的是已删除的 Rust 侧 |
| 3 | **失败被静默吞掉，回执与事实不符** | 未知 `approvalId` 回报 `accepted:true` 而实际零授权；审批票铸造失败被吞而卡片照常弹出；run 的终态写入器落在异常覆盖范围之外，UI 永久显示「运行中」 |

此外还有一条**只在修复过程中才暴露出来**的断层，值得单列，因为它不属于原始审查的任何一类：
**修复自身承诺的不变式缺少第二个读者**。FIX-13 最初把 build input 的 sha256 与构建时间写进 manifest，
但没有任何代码回读它；删掉载荷里的许可证通知也照样 `Verified 710 file(s)` exit 0。
这类缺口不会被「单点实现审查」发现，也不会被「跑一次绿」发现——它需要**对修复本身做证伪**。
本轮为此专门开了 VERIFY-5，并把被证伪的部分当轮修掉（详见第 7 节的登记）。

**审查中最关键的一条发现**（若只看一条）：`native/desktop_host` 的审批入口对「自己账本里不存在的
`approvalId`」曾返回 `{"accepted": true}`，UI 据此显示「已批准一次」，而实际什么都没被授权。
它不构成越权（真正的 effectful 动作仍被 `denied_by_policy` + `approval_ticket` 拒绝），
但**回执语义不可信**。该路径已由 **FIX-9** 改为显式错误，并补上未知 id、白名单、状态与有界等待覆盖。

**修复后状态**：`node scripts/check.mjs` → `check: green`，**14 个步骤全绿**；CTest **13/13**、
控制平面 **149** 个测试、Agent 全量 **167** 个测试、`release artifact tests` **24** 个用例、
shared **142** / 桌面 **325**（324 pass + 1 skip）。**18 项修复全部完成**
（FIX-1 – FIX-18），其中 17 项有反向验证。
**最终门禁由 Lead 在所有写入者停下后统一跑一次**，不采用各任务自报的运行结果作为验收。

---

## 2. 审查基线与方法

- **仓库**：`D:\yukinal`，`HEAD = 736f06a58d07e595fa79ac620d8ce116779bfe4c`（分支 `codex/ui-layout-direction`）
- **平台**：Windows 11，MSVC VS2022 Community `14.44.35207`，CMake generator `NMake Makefiles`
- **模块**：C++ SSH 服务 + C++ WebView 桌面宿主 + Python Agent + React/Vite 桌面 UI + TS 契约包 + 可选 FastAPI 控制平面
- **统一门禁**：`node scripts/check.mjs`（**14 步**；FIX-3 之后新增了 `gate runner guardrail tests`，
  它钉住「显式指定一个无法运行的 CTest 构建目录必须 exit 2，而不是打印 skip 后报绿」这条不变式）
- **第三方关键版本**：libssh2 `f216bbe3`（= `libssh2-1.11.1-1033-gf216bbe3`）、cpp-httplib v0.18.3、内置 OpenSSL 3.5.9、nlohmann_json、sqlite amalgamation、WebView2

三轮共同的**证据标准**：

1. 每条结论必须给 `文件:行` 级证据与可复核的代码片段，不接受仅凭 grep 命中；
2. 能跑的断言都实测（探针脚本、真实二进制、真实 SQLite schema、真 Ed25519 签名）；
3. 每条标注置信度（高 = 逐行确认且本地可复现；中 = 一侧实测、另一侧静态推断；低 = 需特定环境）；
4. **不能静态判定的写「无法判定 + 缺什么证据」，不推测。**

---

## 3. 三轮审查的分工与覆盖

### 第一轮（R1）：广度

三个方向并行，各自做「契约漂移对照 + 生产者/消费者穷尽扫描」。

| 报告 | 范围 | 计数 |
| --- | --- | --- |
| [`r1-native.md`](./review/r1-native.md) | `native/src`、`native/include/yukinal`、`native/tests`、CMake 与第三方依赖语义 | Critical 0 / High 2 / Medium 3 / Low 4 |
| [`r1-desktop.md`](./review/r1-desktop.md) | `native/desktop_host`、`apps/desktop`、`packages/shared`+`agent-sdk`+`provider-sdk` | **Critical 2** / High 4 / Medium 5 / Low 3 |
| [`r1-python.md`](./review/r1-python.md) | `agent`、`services/control-plane`、`scripts`、CI 与配置 | Critical 0 / High 3 / Medium 5 / Low 5 |

### 第二轮（R2）：深度

按子系统纵向追查，要求给出可运行的最小复现。

| 报告 | 焦点 | 产出 |
| --- | --- | --- |
| [`r2-release.md`](./review/r2-release.md) | 构建 → 打包 → 发布链路的完整性边界 | Critical 0 / High 3 / Medium 7 / Low 4 |
| [`r2-control-plane.md`](./review/r2-control-plane.md) | 多租户隔离、节点 Ed25519 签名与重放、WS、迁移 | 27 条操作员路由 + 5 条节点路由矩阵；7 项威胁判定；F-1..F-10；探针编号到 **P115**（报告正文出现 83 个不同编号，其中空缺者是早期脚本重写留下的） |
| [`r2-approval.md`](./review/r2-approval.md) | 审批 / 授权状态机（host ledger ↔ sidecar admission） | Critical 0 / High 2 / Medium 3 / Low 2；T1–T12 状态表 + 11 个静默失败点 |
| [`r2-contract-matrix.md`](./review/r2-contract-matrix.md) | 全部 IPC 命令 × 宿主 / sidecar / UI 三方穷举 | **86 行矩阵（机器生成，非人工列举）**；响应键集差集 |
| [`r2-agent-lifecycle.md`](./review/r2-agent-lifecycle.md) | Agent 运行生命周期、取消传播、并发、resume、审计 | High 4 / Medium 4，其中 **4 项是用户可见的永久挂起** |

R2 的两条方法学亮点：

- **控制平面的探针全部跑在「真实 app + 真实 SQLite schema + 真 Ed25519 签名」上**（探针编号到 P115，
  正文出现 83 个不同编号），
  并且作者自己发现第一版探针有 5 个测量假象（用占位 body 导致的 422 被误读为租户门禁生效），
  重写并加了「启用租户 27/27 全 2xx」的对照组才定稿。
- **契约矩阵完全由脚本生成**：86 条命令取自 `IPC_COMMANDS` 字面量，与 `IPC_SCHEMAS` 一一对应，
  脚本自检 `schemas without a command name = 0`、`commands without a schema = 0`、
  `commands missing from host = 0`。生成器 1347 行，逐字内嵌在报告附录。

### 第三轮（R3）：对抗式证伪

对 R1 全部 **11 条 Critical/High** 独立复核，**默认假定其为假**，只有拿到 file:line 正证据才判「成立」。

| 判定 | 数量 | 条目 |
| --- | --- | --- |
| **成立** | 6 | native H-2；desktop C-1、C-2、H-1、H-4；python H-3 |
| **部分成立** | 3 | native H-1；desktop H-2、H-3 |
| **原报告成立，当前已修复** | 2 | python H-1、H-2 |
| **不成立** | 0 | —— |
| **无法判定** | 0 | —— |

详见 [`r3-verification.md`](./review/r3-verification.md)。

---

## 4. 严重度汇总表

### 4.1 全部条目（按严重度）

| ID | 级别 | 一句话 | R3 判定 | 状态 |
| --- | --- | --- | --- | --- |
| **desktop C-1** | Critical | SSH keyboard-interactive（TOTP / 二次验证）认证链路端到端不可达 | 成立 | **已修 FIX-11**：`auth_prompt` 有了消费者，`server.auth_challenge` 有了生产者，两个命令真正回写 `auth_response`；三处未覆盖的边界见 8.5 |
| **desktop C-2** | Critical | 19 个可订阅通道里 6 个无生产者（另有 3 个声明名同样无生产者），而测试断言它们有 | 成立 | **部分修复 FIX-10**：9 个名字逐条标注保留、测试改为断言「9 这个数字」，**生产者未补**（属 `native/**`）。`agent.thinking` / `agent.usage` 两条经裁决定为**「已如实记录的设计缺口」**：全仓库 `*.py` 零 emit（含测试），补 producer 只能伪造数据；`event-vocabulary.test.ts:145-158` 钉住「9 名逐条」，改名/删名会红。将来接线点在**模型适配层**，不是加一个 emitter |
| **desktop H-1** | High | `agent_status.restart` 与整套自动恢复：契约、UI、fixture 齐全，宿主侧无生产者也无 supervisor | 成立 | **已诚实标注 FIX-10**（契约保留 + 显式说明无生产者）；恢复功能本体未做 |
| **desktop H-2** | High | verbatim 转发进 `z.strictObject`：sidecar 多一个字段就能把 UI 打崩 | 部分成立（现状未崩） | **已修 FIX-10**：13 条转发行显式容错 + `unknownResponseKeys()` 报出漂移，其余响应仍严格 |
| **desktop H-3** | High | 审批 `decision` 在宿主入口无白名单；未知 `approvalId` 被当成成功 | 部分成立（无越权） | **已修 FIX-9**：未知 id 显式报错，decision 白名单化 |
| **desktop H-4** | High | `mcp_server_list` 对运行中的服务器报 `running:false`，且 MCP 状态对象有两个形状 | 成立 | **已修 FIX-9**：列表返回真实状态 |
| **native H-1** | High | `exit_code` 无法区分「退出码 0」与「未收到 exit-status」 | 部分成立 | **已修 FIX-8**：改用 `libssh2_channel_has_exit_status()`，修复前二进制上复现假成功 / 假失败 / `null` 三种错误 |
| **native H-2** | High | Linux 上内置 OpenSSL 的 TLS 写路径未屏蔽 SIGPIPE，对端 RST 可终止进程 | 成立 | **已修 FIX-8（源码级）**：`#ifndef _WIN32` 下忽略 SIGPIPE；**本机无 Linux，行为级未验证** |
| **python H-1** | High | Agent 测试只有 100/153 被门禁覆盖，18 个测试在任何路径下都不执行 | 成立（已修复） | **已修 FIX-3** |
| **python H-2** | High | 控制平面 129 个测试在 CI 中从不运行（venv 不存在即静默跳过） | 成立（已修复） | **已修 FIX-3** |
| **python H-3** | High | `policyId` 契约在 API 边界被拒绝，选择任何内建策略都会让 run 启动失败 | 成立 | **已修 FIX-7 + FIX-15**：Agent 侧维持 `-32602`（该节点**如实上报** `permissionPolicyIds: []`，拒绝是正确的边界），桌面侧停止发送该字段并把选择区改为不可选 |
| **release H-1** | High | `pnpm package` 产出的载荷根本不含 UI 资源，无法作为桌面应用启动 | —— | **已修 FIX-4** |
| **release H-2** | High | `verifyReleaseManifest` 只复核清单列出的路径，新增文件与整块外来载荷目录不可见 | —— | **已修 FIX-4** |
| **release H-3** | High | `--manifest` 会把磁盘现状「祝福」进契约 | —— | 未修（设计如此）；但 `inputs` 段自 **FIX-13** 起不再享有该自由 |
| **approval R2-H-1** | High | 宿主账本不认识的 `approvalId` 仍回报 `{"accepted": true}`，UI 显示「已批准一次」而实际零授权 | 与 R3 同向 | **已修 FIX-9**：未知 id 显式报错 |
| **approval R2-H-2** | High | `consume` 在 `Responding` 相位无上界自旋且不感知取消；4 个停驻请求即可占满 4 个宿主 worker | R3 列为「未能验证」→ 本轮实测补齐 | **已修 FIX-9**：等待有 180 s 上界并取消响应 |
| **lifecycle R2E-H-1..H-4** | High×4 | 4 条让 run **永久停在运行中**的机制（详见 4.2） | —— | **已修 FIX-7**：done-callback 兜底 + 前缀纳入 `try` + 终态 `_emit` 与审计解耦；Agent `153 → 164`，Lead 独立复现 11 条新用例中 9 条在修复前变红 |
| **control-plane P0-1** | High | 签名规范串不含 node id + `public_key` 无唯一约束 → 同一把公钥登记进两个租户后可跨租户读写 | —— | **已修 FIX-6**（破坏性客户端契约变更，升级说明见控制平面 README） |
| **control-plane P0-2** | High | `unknown→running` 绕过派发门禁（纯 HTTP，违反「deny 绝对」的自身不变量） | —— | **已修 FIX-6** |
| **control-plane P0-3** | High | 失败的请求不烧 nonce → 300 s 窗口内无条件重放 | —— | **已修 FIX-5** |
| **control-plane P0-4** | High | 禁用租户对 10 条含 `{tenant_id}` 的路由无效（其中 `approvals/expire` 完全无租户门禁） | —— | **已修 FIX-5 + FIX-6**：24/24 `{tenant_id}` 路由对禁用租户均 404，实测泄漏 6（HEAD 9）→ **0** |
| 其余 Medium / Low | Medium / Low | 见各报告正文（合计 Medium 27+、Low 21+） | —— | 多数未修 |

### 4.2 「永久停在运行中」的四条机制

这是 R2 生命周期审查唯一不可替代的产出，也是**用户可见**的严重缺陷：UI 只有两条依赖
**进程死亡**的兜底信号（`useAgentRun.ts:296-310`、`:318-335`），因此「进程存活 + run 不再推进」
就是永久挂起。按根因归并后是 **3 个根因 / 4 条机制**：

| 机制 | 根因 | 为什么终态不可达 | 实测 |
| --- | --- | --- | --- |
| H1 取消落在协程首步之前 | `task.cancel()` 把一个尚未开始的协程标记为 cancelled，**协程体从不运行** | `_execute_run` 的 `try`/`except`/`finally` 与 `_execute_session_run` 的 `finally` **全部不执行**，`_completed_runs` / `admission.completed` 永不写入 | `task cancelled: True`、`events: []`、`_active_runs: ['run-1']`、重试 `duplicate: True` |
| H2 前置区抛异常 | `_execute_session_run` 的前置区（读 context / memory / `record_event`）在 `try:` **之外** | `_execute_run` 的 `finally` 仍执行（所以 `_active_runs` 被清空，**连「停止」入口都没有**），但唯一写终态账目的位置不执行；异常成为「Task exception was never retrieved」 | `events: []`、`_active_runs: []`、`admission.completed: False`、`turn status: running` |
| H2b 前置区的 `_emit` 抛错 | 同 H2（审计写入失败，如磁盘满 / sqlite 锁） | 同 H2 | `notified events: []`、`turn status: running` |
| H3 终态 `_emit` 抛错 | `_emit` 先 `record_event` 再发通知；审计抛错则通知根本不发，外层 `except` 再次 `_emit` 又抛一次 | `finally` 仍执行（磁盘状态「干净」）但 UI 永远等不到终态 | `notified events: ['agent.started','agent.text']`（**无 completed**），而 `turn status: succeeded` —— 磁盘说成功，UI 认为还在跑 |

四条下 `agent.run.start` 都**成功返回** `{"started": true, ...}`，`setRunning(true)` 早已生效；
此后无终态事件、`sidecarRunning` 仍为 `true`、重发被判 `"busy"`、停止得到
「停止请求未被接受，请重试。」，切换会话也不清 `running`。**唯一恢复手段是重启 sidecar 进程。**

`delivery: "sync"` 不是独立第 5 项，而是上述机制的**放大与稳定触发路径**：sync 路径下
`_admit_run` 会 `await task`，于是「用户按停止」正好落在 start RPC 还挂着的时候，H1 从
「需要极短竞速」变成「稳定可复现」。它另有一个不依赖取消的形态：start RPC 整个 run 期间都不返回，
而宿主侧超时是 15 分钟，agent 预算只有 ≤120 秒。

---

## 5. 最值得优先修的 8 条

按「能挡住真实风险 × 改动成本」排序。**这一节是修复开始时的排序，保留原样**，以便读者看到
实际处理顺序与当时的判断是否一致；每行末尾补上当前的归属与状态。

| 序 | 条目 | 为什么排这里 | 成本 | 当前状态 |
| --- | --- | --- | --- | --- |
| 1 | **审批入口未知 `approvalId` 返回显式错误**（approval R2-H-1） | 唯一一条「UI 明确说成功、实际零授权」的路径；改动最小收益最大 | **S** | **已修 FIX-9** |
| 2 | **run 终态写入器移入异常覆盖范围**（lifecycle H1/H2/H2b/H3） | 4 条永久挂起、用户可见、只能靠重启进程恢复；3 个根因各一处修复 | **S–M** | **已修 FIX-7** |
| 3 | **控制平面：签名纳入 node id 或给 `public_key` 加全局唯一约束**（P0-1） | 唯一一条可跨租户**读写**的路径（前置条件：同一把公钥登记进两个租户，schema 与 API 都允许） | **M** | **已修 FIX-6**（两项都做了） |
| 4 | **非 404 的失败也要烧 nonce**（P0-3） | **已完成**（FIX-5）；无条件重放是 0 前置条件的可利用路径 | 已完成 | **已修 FIX-5**；其副作用（nonce 无上限）由 **FIX-6** 补上 |
| 5 | **`unknown→running` 纳入派发门禁**（P0-2） | 纯 HTTP 即可绕过「deny 绝对」的自身不变量 | **S–M** | **已修 FIX-6** |
| 6 | **给 13 条 verbatim 转发命令加响应闸门**（contract-matrix B1） | 把「sidecar 加一个字段就崩」变回「加字段无感」；当前 12 条处于「将来即崩」状态 | **M** | **已修 FIX-10**（选了「容错 + 漂移可观测」，非宿主裁剪） |
| 7 | **补齐 6 个死通道的生产者，或让契约诚实**（desktop C-2） | 测试断言的是已删除的 Rust 侧；不改则「测试在为一个不存在的实现对账」 | **M**（补实现）或 **S**（改断言） | **部分 FIX-10**：9 个名字逐条标注 + 测试改为断言数字；**补生产者未做** |
| 8 | **宿主接线 keyboard-interactive**（desktop C-1） | 当前桌面宿主上 TOTP / 二次验证**不可能完成**；服务层已实现且有测试，缺口纯在宿主，**修复成本比 R1 估计的低** | **M** | **已修 FIX-11** |

**排序与结果对照**：第 2、3、4、5、6 条已全部交付，第 7 条只交付了「让契约诚实」的那一半，
第 1、8 条也都已交付（**FIX-9** / **FIX-11**）。**当时排第一的审批回执问题，实际是最后一批才做** —— 它落在
`native/desktop_host`，与 FIX-11 共用写作用域，需要先等 native 构建释放。排序不变，但顺序被
并发约束改写，这一点值得记下来：**优先级不等于可并行度**。

另外有一条**不在这 8 条里、却是修复过程中才发现的 High**：`policyId`。Agent 侧拒绝它是正确的
（该节点如实上报 `permissionPolicyIds: []`），但桌面 UI 会把用户选的策略放进请求，于是
**用户一选策略 run 就启动失败**。它不在原始排序里，是因为原始审查只看到「API 边界拒绝」这一半，
没有把 UI 的发送路径接起来。这条现由 **FIX-15** 处置。**「契约的两端要一起看」是这轮最实际的方法论收获。**

---

## 6. R3 推翻或修正的断言

R3 的价值在于它真的推翻了几条。这些修正**已并入**本报告，读者引用 R1 时应使用下表口径。

| # | R1 原断言 | R3 修正 | 修正后口径 |
| --- | --- | --- | --- |
| 1 | native H-1 子项 2：「`exit_code < 0` 守卫是**死代码**」 | **推翻。** libssh2 的 `packet.c` 对收到的 32 位状态做**无条件强转**，`0x80000000` 会变成负数；R3 实测 `exit-status=0x80000000` → `502 / result_unknown`，守卫**可达** | 「**可达但语义错误**」：真正的缺陷是「未收到 exit-status」与「退出码 0」不可区分（假成功），以及合法的大退出码被误报为失败（假失败）。R1 正文写对了，标题与摘要写反了 |
| 2 | desktop H-3：「未知 `approvalId` 被当成**成功** / 可**越权**拿会话级授权」 | **推翻越权部分。** 宿主的 `begin_response` 对未知 ID 返回空，`finish` 是空操作，**没有授权发生**；`approve_session` 在宿主与 sidecar **双双硬拒** | 降级为「**静默 no-op + 误导性回执**」。这是 R2 的 R2-H-1，定级 High 但原因是**回执不可信**，不是越权 |
| 3 | desktop H-2：「sidecar 多一个字段就把 UI 打崩」的**现状性暗示** | **推翻现状性。** R3 按生产端逐字字段集重放 20 组：17 通过 / 4 拒绝，其中 3 个拒绝是故意注入的漂移字段，唯一「非漂移」那例是抄错了生产者 | 真实风险是「**将来加字段即崩**」，当前不存在会被拒的真实响应。R2 契约矩阵独立复算确认：会抛 **0** 条 / 「未来加字段即抛」12 条 |
| 4 | desktop C-1 的定性（隐含「一类认证方式在服务层也不可用」） | **修正为宿主接线缺口。** R3 补了 R1 缺的正面证据：`ctest -R ssh_mock_integration|agent_ssh_mock_integration` **2/2 Passed**，真实 `yukinal-ssh-service.exe` 走完 `auth_prompt → auth_response → ready → keyboard-interactive-success` | 服务层**已实现且有测试**，缺口纯在 `native/desktop_host`（该目录 `auth_prompt`/`auth_response` 字面量计数为 **0**）。因此**修复成本下调** |

另有一处 R3 的自我纠正值得记录：终端路径的 `exit_code: null` **只在客户端主动断开**时才出现，
让**服务器侧**关闭 channel 会得到 `exit_code: 0`。因此 `null` **不能**当作「服务器未发 exit-status」
的信号。

---

## 7. 已修复清单

**18 项全部完成**，每一项都有实测验收与反向验证。详细证据见 [`review-fixes.md`](./review-fixes.md)。

| 修复 | 内容 | 关键验收 |
| --- | --- | --- |
| **FIX-1** | 基线门禁失败（CTest 2 项失败） | 根因是**陈旧构建树**而非源码缺陷；重建后 15/15 |
| **FIX-2** | 文档与仓库卫生四处不一致 | 仓库不再依赖未跟踪的任务输入文件；README/CONTRIBUTING 如实描述门禁覆盖（含「无 lint」） |
| **FIX-3** | 门禁与 CI 加固 | Agent 全量 **153** 测试入闸（原 100）；控制平面「静默绿」改为硬失败；CI 装控制平面依赖并设 `REQUIRED=1`；CTest 15→**13**（去 2 个重复注册，**净增 18 个覆盖**） |
| **FIX-4** | 打包链路（release H-1、H-2） | 载荷补入 UI（711 文件，含 `ui/index.html`）；`--verify` 反向枚举磁盘未声明文件；拒绝符号链接 / NTFS 联接；`isSafeRelativePath` 拒绝 Win32 保留名、尾点/尾空格、ADS、UNC；测试 11→**18** |
| **FIX-5** | 控制平面两条 P0（P0-3、`expire` 门禁） | `expire` 对不存在/已禁用租户统一 404 且零副作用；nonce 改**独立提交的短事务**并前移到验签之前，两类重放均被覆盖；测试 129→**139** |
| **FIX-12** | release M-5：许可证通知按**构建 venv** 枚举 | 通知 `418306 B / 85 个发行版 → 46086 B / 21 个`，幻影包 `yukinal-agent-node` 残留 **0**；空载荷与「仅 exe 载荷」硬失败；收录规则改由 `License-File` 驱动后 Apache-2.0 正文 `1 → 9` 处；路径相对化后绝对/相对 `--payload` 输出**逐字节相同** |
| **FIX-13** | release M-1：旧清单把整份载荷归因于 `HEAD` | 陈旧产物 **exit 1 且不创建输出目录**；4 条 build input 的 sha256/时间/修订/`dirty` 入清单；`inputs` 段被 `--verify` 与 `--manifest` 双向校验（篡改 sha256 或重复 role 都无法通过，也无法被「刷新」洗白）；测试 18→**24** |
| **FIX-14** | release L-3：`clean` 不覆盖 `target/` 与冻结 Agent | 恰好 7 条命中、0 条被跟踪文件、`.gitkeep` 幸存；glob 显式 `cwd` 并改用 `join()`（`new URL()` 对 `#`/`?`/`%` 会解析到别的路径） |
| **FIX-6** | control-plane P0-1 / P0-2 / P0-4 剩余 / nonce 上限 | 签名规范串纳入 **node id** 且 `public_key` 全局唯一（新迁移，遇重复**拒绝升级并留下指纹**）；实测跨租户同签名 `true → 401`；禁用租户泄漏 `6（HEAD 9）→ 0`，24/24 `{tenant_id}` 路由与路由表**集合相等**后逐一驱动；100 000 字符 nonce 从**落库**变 0 行，200 次垃圾签名从 200 行降到**恰好 128 行**；测试 139→**148**；逐项还原 5/2/7/2 红 |
| **FIX-8** | native `exit_code` 语义（H-1）+ `SIGPIPE`（H-2） | 用 `libssh2_channel_has_exit_status()` 区分「退出码 0」与「未收到」；在**修复前二进制**上分别复现**假成功**（未发 exit-status 报成功 0）、**假失败**（`0x80000000` 被判 unknown）、**状态被抹成 `null`**；CTest 5/5、63 用例 0 失败；SIGPIPE 只交付**源码级护栏**并如实标注（本机无 Linux，无法行为级验证） |
| **FIX-10** | 契约诚实化：13 条 verbatim 转发响应 / 9 个零生产者通道 / Rust-Tauri 因果依据 | 选「容错 + 漂移可观测」而非宿主裁剪：13 条转发行显式列出、其余响应仍 `strictObject`，并新增 `unknownResponseKeys()` 报出未声明字段；shared 135→**142**、desktop 323→**325**；把容错构造临时恒等化后 shared **4 红** / desktop **1 红**；9 个零生产者事件名**逐条标注保留**、测试改断言「9 这个数字」 |
| **FIX-7** | Agent 四条「永久停在运行中」（H1 取消落在首步之前 / H2·H2b 前置区在 `try` 外 / H3 终态 `_emit` 抛错）+ H4 的 sync 放大后果 | Agent 153→**164**（skip 数不增）；**Lead 独立复现**：把 HEAD 的 `sidecar.py` 导出到 `target/lead-verify/src-old/` 并仅替换该模块后，新增 11 条 `FAILED (failures=8, errors=1)`；每条新用例同时断言「终态事件到达 sink」+「磁盘/内存终态已写」+「恰好一条终态」；两条刻意在修复前即绿（policyId 边界不许回归、中途取消仍须生效） |
| **FIX-11** | 桌面宿主接通 keyboard-interactive（最后一条 Critical） | 服务端的 `auth_prompt` 帧终于有了消费者：新增 `AuthChallenges` 状态机（一次性、TTL 180 s、条数必须等于期望 —— 服务端在条数不符时会把会话标记 `invalid`，所以宿主必须先校验）、`handle_frame` 发出 `server.auth_challenge`、两个命令真正回写 `auth_response` 到 PTY stdin；未知/过期/已答/条数不符返回 `AUTH_CHALLENGE_UNKNOWN` 而不是谎报 `accepted:false`。CTest **13/13**；两个变异（去掉条数校验 / 挑战不复用一次）各自变红 |

修复的**共同特征**：每条都附了**反向验证**（故意让门禁失败以证明它会失败），而不是只跑一次绿。
例如 FIX-3 用坏解释器 + `REQUIRED=1` 证明 exit 2；FIX-4 往载荷里塞 `BACKDOOR.exe` 证明 `--verify`
非零退出、证明联接被拒且不被跟随；FIX-5 把修复前源码还原后跑同一套测试，得到
`FAILED (failures=7)`，7 条恰好是本轮新增/加强的用例。

**FIX-12/13/14 是被自己的反向验证改写过的**：它们落地后由 `fixer-gate2` 做对抗式证伪
（`target/verify-fix5/VERIFY-5-report.md`），结论「FIX-13 的 provenance 那一半基本是 theatre」
——写进去从不回读、可被 `--manifest` 洗白、删掉载荷内的许可证通知整个绕过。这三条随后的加固
（`inputs` 与载荷逐条对账、`launch.agentEntry` 强制通知同目录存在、通知-载荷发行版集合对账）
都是**证伪的产物**，不是原计划的一部分。这段过程本身值得保留：一条修复是否成立，
取决于它承诺的不变式是否有**第二个读者**在强制，而不取决于它是否成功执行了一次。

**第二批修复也被同样证伪过，而且推翻了我们自己的两条说法**（`target/verify-final/VERIFY-FINAL-report.md`）：

| 被推翻的说法 | 实测反例 | 处置 |
| --- | --- | --- |
| FIX-6「每节点未过期 nonce 上限 128」是一个 security bound | `COUNT(*) → 判断 → await flush` 之间存在读-写间隙：真 app + 真签名 + `Barrier` 对齐，**余量 4 / 并发 64 → 表里 188 行**（声称 128）；最小反例 `124 → 140`。claim 在**验签之前**，未认证方开 N 条连接即可写 `128+N` 行 | **FIX-17** 重做（上限只作用于未验证残留 + 让计数原子），并把 README 与 docstring 的过度声称降级 |
| 同上：该上限对**合法**节点也是错的 | 200 次合法唯一 nonce → **128 接受 / 72 拒绝**，拒绝体与真坏签名**逐字节相同**；稳态 `0.43 req/s`，每秒心跳的节点 128 秒后被限流，却被告知「签名无效」——把可恢复的运维事件伪装成密钥故障 | 同上 |
| FIX-7「终态恰好一次」有测试守护 | 删掉 `_terminal_emitted` = **0 条红**；删掉「投递成功才记账」= **0 条红**（红集与基线完全相同）。测试名写着「恰好一次」，验的却是「调用图只走一次」；手工双调证明守卫确实承重，但**没有读者** | **已修 FIX-18**（只动测试，`sidecar.py` 逐字节未改）：让测试观测**投递要约次数**，新增 3 条用例钉住三根梁 —— AG-1 变异 **1 条红**（此前 0）、AG-3 **3 条红**（此前 0）、AG-4 只红 1 条（证明它钉的是另一根梁）。Agent **164 → 167** |

**同一次证伪还带来一次自我修正，这段保留下来**：它先判定「FIX-8 的 terminal `closed` 帧没有
行为读者」（`M4` 变异 = 0 条红），随后自己推翻——读者本来就在 `scripts/test_ssh_mock.py:926-932`
（`0x80000000 → -2147483648`、无 exit-status → `None`），只是不在 C++ 单测里；FIX-16 又在便宜目标
上补了帧形状断言，用同一份 `M4` 变异体复算确认会变红，缺口闭合。**做变异的人也需要被复核**。

### 第二批已全部交付

第二批共 13 项（FIX-6 – FIX-11、FIX-15 – FIX-18），**全部完成**，各自的验收证据见上表与
[`review-fixes.md`](./review-fixes.md) 第 5–7 节。**没有遗留未开工的修复项**；下一节列的是
「已知的边界与未覆盖之处」，不是未修的缺陷。

### 修复后门禁

```
$ node scripts/check.mjs
check: green          exit=0          （14 个步骤全绿）
```

| 步骤 | 最终实测（`check: green`、`exit=0`） |
| --- | --- |
| publication hygiene | green（609 tracked files） |
| secret scan | green（633 files checked） |
| cross-platform command runner tests | 7 pass |
| release artifact tests | 24 pass |
| gate runner guardrail tests | 2 pass（证明「显式指定坏的 CTest 构建目录」会让门禁 exit 2，而不是打印 skip 后绿） |
| docs links | green（39 markdown files, 207 links） |
| configured native CTest suite | `100% tests passed out of 13` |
| control plane tests (SQLite) | `Ran 149 tests` OK |
| Python Agent tests (full unittest discovery) | `Ran 167 tests` OK（skipped=4） |
| build contract libs / typecheck / unit tests | shared 142、provider-sdk 4、agent-sdk 5、desktop 325（324 pass + 1 skip） |
| desktop bundle (vite build) | OK |
| Python Agent source sidecar smoke | green（含干净关闭） |

相对第一批完成时：控制平面 139 → **149**、Agent 153 → **167**、shared 135 → **142**、
desktop 322 → **325**，CTest 目标数与用例只增不减。

---

## 8. 已知未修 / 明确不做

诚实列出**没有**修的东西，以免读者把「绿」当成「无缺陷」。

### 8.1 第一批**未修**，现已由第二批处置

第一轮（FIX-1 – FIX-5）刻意**没有**顺手改这些：它们的修法会变更签名规范、数据库约束或状态机
语义，混在文档卫生与门禁加固里做会让每次改动都难以验收。它们随后各有独立任务与反向验证，
**除最后一行外全部已交付**：

| 条目 | 第一批为什么没顺手修 | 第二批归属与结果 |
| --- | --- | --- |
| **P0-1 签名不绑定 node id / `public_key` 无唯一约束** | 修法涉及签名规范与数据库约束，会让既有节点凭据与部署失效，需要迁移方案 | **FIX-6 已修**；随后被 FIX-17 补上「上限不是真界」的漏洞 |
| **P0-2 `unknown→running` 绕过派发门禁** | 涉及任务状态机对「哪些边进入 running」的定义 | **FIX-6 已修** |
| 审批不绑定 tenant / node / fingerprint | 属于授权模型调整 | 部分在 **FIX-6**（tenant 门禁）与 **FIX-9**（回执诚实化） |
| 禁用租户的另外 **9** 条路由 | FIX-5 只修了 `approvals/expire` | **FIX-6 已修**（24/24 路由逐一驱动，泄漏 6→0） |
| Agent 四条「永久停在运行中」 | 需要把终态记账从 `finally` 挪到一定执行的路径 | **FIX-7 已修**；测试债由 **FIX-18** 补 |
| native `exit_code` 语义 / SIGPIPE | 需要重编 native 目标并新增 mock 场景 | **FIX-8 已修**；SIGPIPE 的「第二个读者」由 **FIX-16** 补（行为级仍未闭环） |
| 审批回执谎报成功 / `consume` 占用宿主 worker | 与 FIX-5 同属「静默成功」，但改的是 C++ 宿主 | **FIX-9 已修** |
| 13 条 verbatim 命令的响应闸门 / 6 个死通道 | 需要先决定是「让契约诚实」还是「让实现补齐」 | **FIX-10 已修**（选「容错 + 漂移可观测」） |
| `policyId` 边界拒绝导致「选策略即启动失败」 | 原始审查只看到 API 边界这一半 | **FIX-15 已修**（桌面侧不再发送） |
| **keyboard-interactive 宿主接线** | 服务层已实现并有测试，缺口纯在宿主，属一条**完整功能** | **FIX-11 已修**（三处未覆盖的边界见 8.5） |

### 8.2 设计如此，需要的是流程而不是代码

- **release H-3**：`--manifest` 会把手动加入的文件「祝福」成已验证。这是 `--manifest` 的**固有语义**
  （重建清单时就该反映磁盘现状）；防线应放在「谁能跑 `--manifest`」。
- **release 新行为**：`--verify` 现在枚举整个输出根，所以升级版本号后输出根下遗留的旧载荷目录
  会让校验失败。这是**期望行为**（旧载荷不应与新清单混放），但它是一个真实的行为变更，
  已在 `docs/release.md` 写成显式注意事项。

### 8.3 FIX-5 引入的新代价（已由 FIX-6 承接）

占用 nonce 前移到验签之前之后，**任何知道 node id 的人**都可以用垃圾签名写 nonce 行
（实测 200 次 → 200 行、7.0 ms/次），且超长 nonce 会原样落库（SQLite 不强制长度，
PostgreSQL 下会变成驱动层错误）。这一条**不是第一批能顺手修的**——它需要一个显式的长度上限
与按节点的未过期 nonce 计数上限，而这两者都会改变已认证请求的接受边界，因此单列为 **FIX-6**
的一项（要求超长 nonce 返回与验签失败**完全相同**的 `Unauthorized`，避免变成长度预言机）。

### 8.4 其他未修的 Medium / Low

各报告正文完整列出，典型的有：`service_status` 未加引号、`SshResources` 析构半释放、
手写单引号拼接、`sftp_read` 缺截断标志、`pnpm check` **无 lint / 格式化 / 静态分析**
（这一点已在 README 与 CONTRIBUTING 里如实写明，避免读者误以为覆盖了）。
`mcp_server_list` 硬编码 `running:false` 与事件词表断言已删除的 Rust 侧，已分别由
**FIX-9** 与 **FIX-10** 承接。

---

### 8.5 FIX-11 已落地，但有三处边界没有覆盖

FIX-11 修的是 R1-desktop C-1（这台桌面宿主上 TOTP / 二次验证不可能完成）。修复已合入并通过
CTest，但**不要**把「已修」读成「端到端验证过」：

- **没有端到端 PTY 行为测试**：要跑通全链路需要一个会发 keyboard-interactive 挑战的 SSH 服务器。
  已覆盖的是挑战状态机（纯逻辑，一次性 / TTL / 条数 / 会话关闭清理）、事件载荷与
  `server.auth_challenge` schema 的逐字段一致、以及 IPC 层对未知 id 的显式拒绝。
  **「`auth_response` 字节真的写进 stdin」这一步没有针对性的字节断言**，只有代码路径与
  `write_frame` 的既有测试作依据。
- **`username` / `host` 是显示值**：来自启动会话时的 target 元数据，取不到时退化为 prompt 的
  `name`、再退化为 `"unknown"` / `serverId`（schema 要求非空字符串）。它们不参与认证判定。
- **UI 侧未改也未被点过**：`apps/desktop` 本来就订阅该事件并按 `{authId, responses}` 回答，
  与宿主现在发出的载荷一致；但没有在真实 WebView 里手动完成过一次二次验证。

## 9. 无法判定与需要外部环境的部分

| 项 | 缺什么证据 |
| --- | --- |
| **native H-2 的现场触发** | 本机 Windows 无 `signal.SIGPIPE`、WSL 无可用发行版、无 Docker/Podman、无远程主机，**无法在 Linux 上实测 RST → SIGPIPE 终止**。静态链经**独立复核**（不引用他人结论）：内置 OpenSSL 3.5.9 的 `crypto/bio/bss_sock.c:156` → `writesocket` → `include/internal/sockets.h:202` 的 `#define writesocket(s,b,n) write((s),(b),(n))`（Linux 走这一支），全树 `MSG_NOSIGNAL` **0 命中**；`native/` 全树 `signal`/`sigaction` 真实命中**仅 1 处**（`native/src/main.cpp:57`），且无 `exec*`/`fork`/`popen`/`system`/`posix_spawn`（**0 命中**，因此「后面会被 exec 重置」的反证不成立）。FIX-8 交付**源码级护栏**（`native/tests/test_network.cpp:258`/`:370`，含非空匹配护栏与行号断言，5 个变异全部变红），并明确标注**它不是行为测试**。**仍需**一台 Linux runner（glibc 的 `MSG_NOSIGNAL` 语义 + 静态链接的内置 OpenSSL）：监听 socket、客户端 `SO_LINGER{1,0}` + `close()` 发 RST、服务端在同一 fd 上继续 `SSL_write`，断言 (a) 进程**未**被信号终止、(b) 写调用返回 `EPIPE`/`ECONNRESET`；CI 落法是 `check.yml` 加一个 `runs-on: ubuntu-latest` job 跑该注入用例。在该 job 变绿前，H-2 保持「已修但行为级未验证」 |
| **对照：`native/desktop_host` 为什么**不**需要同一处理** | `cpp-httplib` 0.18.3 的 `signal(SIGPIPE, SIG_IGN)` 位于 `inline Server::Server()` **构造函数体内**（`httplib.h:6083-6089`），而桌面宿主在 `native/desktop_host/src/main.cpp:374` **无条件**构造 `UiServer`（其 `:196` 的 `httplib::Server server_` 成员构造即执行，早于 `:380` 的 `Database`、`:435` 的 sidecar 与任何管道写）；`--help`/`--self-check` 在 `:368-373` 提前返回且不写管道。反过来，`--terminal`/`--sftp-*` worker 模式**永远不构造 `Server`**，那正是 `native/src/main.cpp` 必须显式安装的场景。**两处需要保护的路径互补、不重复。**<br>**但这里有一处不对称值得记录**：桌面宿主的保护是**传递依赖**（`native/desktop_host/**` 对 `SIGPIPE`/`std::signal` 命中数为 **0**，它自己不表达任何意图），所以哪天有人把 `httplib::Server` 换掉、或把 `UiServer ui_server;` 改成惰性构造挪到 `:435` 之后，SIGPIPE 会**静默**回到默认终止行为，而**没有任何测试会变红**。`native/src` 那一侧正因为 worker 模式不构造 `Server` 才必须自装，并有源码级护栏钉住；桌主侧则「自装是冗余、不装又无人守护」。彻底了结的最小成本是在桌主侧也加一条**非行为**的字符串断言（钉住 `:374` 早于 `:435` 这个顺序），属 `native/desktop_host/**`，**本轮未做** |
| **native H-1 的真实 sshd 触发概率** | 仅在 paramiko mock 上证明了「服务器不发 exit-status 会得到假成功」与「大退出码被误判为 unknown」，真实 OpenSSH 行为未验证。FIX-8 的差异回归用的是按 HEAD 源码单独编译的修复前二进制，证据强度高于「读代码推断」，但仍不等价于真实 sshd |
| **控制平面 9 项** | PostgreSQL/asyncpg 的 FK / 列长 / 部分索引行为、多 worker 竞态、真实网络 WS 背压、生产 TLS 终止、既有部署的迁移回填策略。**FIX-6 的 24 条禁用租户路由测试、唯一约束与新迁移全部只在本机 SQLite 上验证**；`uq_nodes_public_key` 在 PostgreSQL 上的并发插入路径未被覆盖 |
| **FIX-6 的客户端契约升级** | 签名规范串变了，节点驱动必须签自己的 node id。已确认仓库内无受影响消费方（`agent/ native/ packages/ apps/ scripts/` 对 `X-Yukinal-*` / `sign_headers` / `canonical_request` 命中 **0**），但**外部驱动与版本号策略**未验证、也未决定 |
| **完整 C++ 重构建 + 安装后验收** | 本机 WebView2 SDK `1.0.1150.38` 低于 `native/CMakeLists.txt` 要求的 `1.0.2903.40`，完整 `cmake --build` 无法在本机跑通（**环境限制，非源码缺陷**）；逐 target 构建可用 |
| **真实 OpenSSH 容器验收 / 真实云 Provider live** | 本机无 Docker、无 API key |
| **`activity.created` 推荐发射点的线程安全性** | 属修复设计，未验证 |
| **FIX-12 的真实 PyInstaller 失败路径** | `build-python-agent.mjs` 的清理分支是在副本树里用 `PYTHONPATH` stub 验证的，不还原真实 `_internal/` 布局，也不还原「写到一半崩」；因此只能说「清理发生在构建前且退出路径 fail-closed」，不能说「真实失败构建不留旧通知」 |
| **载荷内许可证文本的字节级完整性** | 通知现在与**发行版集合**对账，但不与每个 `.dist-info` 的实际许可证字节对账；超大文件省略（`>2 MiB`）与重复文本共享仍是**告知性**输出，两种情形在当前 21 个发行版里都未出现（0 / 5 条），因此没有被真实样本覆盖 |

---

## 10. 第四轮（2026-10-19）

前三轮之后又做了一轮独立审查（四路只读审计 + 三路修复 + 门禁加固），新增 26 项确认缺陷与
FIX-19 – FIX-44，连同「这一轮改变了哪些判断」「仍然未修 / 明确不做」「变异验证」都记录在
[`review-round4.md`](./review-round4.md)。本文件（第 1–9 节）保持为前三轮的结论，**不被第四轮
覆盖**；两批修复说明的关系是：

- [`review-fixes.md`](./review-fixes.md)：FIX-1 – FIX-18（第一、二批）
- [`review-round4.md`](./review-round4.md)：FIX-19 – FIX-44（第三批）

---

## 11. 可复核性

- **原始报告**：10 份，全部在 [`docs/review/`](./review/README.md)，含 `file:line` 证据、
  探针脚本源码或逐字命令行、实测转录。全部**未跟踪**（`?? docs/review/`），是过程证据不是仓库文档。
- **探针与产物**：控制在 `target/`、`services/control-plane/.tmp/`、`%TEMP%`，均被 `.gitignore` 覆盖；
  审查者未借探针改动任何被审查文件（各报告均有 `git status --porcelain <范围>` 为空的声明）。
  **复审过一处真实违规**：FIX-10 的写者在 `packages/shared/` 下留了 9 个未跟踪的
  `*.probe.tmp.ts` / `*.probe.test.ts`，其中测试文件会被 workspace 的 `**/*.test.ts` 收集、
  从而在任何完整门禁里造成假红；已要求其删除并把「修复前会红」的证据移入 `target/task19/`。
  这条记录保留下来，因为「临时探针放哪里」是有实际后果的纪律，不是形式要求。
- **文档门禁**：把 10 份原始报告与两份交付文档一并临时纳入扫描集合后
  （`git add -N docs/review docs/review-report.md docs/review-fixes.md`），
  `publication hygiene`（621 tracked）与 `secret scan`（625 files）仍为 green —— 即这些报告不含
  私有阶段号、未发布文档指针或形似凭据的字面量。**该组合第一次运行时确实红了一次**
  （`review-report.md` 里一处章节符号引用被 `§NN` 规则命中），修正后转绿；这也是把它当成
  收尾步骤而不是一次性动作的理由。
- **写入面**：`git diff --stat` 现为 **99 个被跟踪文件、+3858 / −489 行**（含 FIX-6/8/10 的
  大批改动；FIX-7/9/11 合入后还会上升），新增 `docs/review/`、两份汇总结论文档、
  `scripts/lib/gate.test.mjs`、`native/src/exit_status.hpp` 与一个 Alembic 迁移。
  `ssh_agent_development_brief.md` 是外部任务输入，**保持未跟踪、不提交**。

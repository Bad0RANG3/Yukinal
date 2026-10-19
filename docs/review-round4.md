# Yukinal 第四轮审查与第三批修复（2026-10-19）

配套文档：[三轮审查总报告](./review-report.md) ｜ [前两批修复说明](./review-fixes.md) ｜ [原始报告索引](./review/README.md)

本文件记录**第四轮独立审查**发现的 26 项确认缺陷，以及第三批修复（**FIX-19 – FIX-44**）的
改动内容、设计取舍与验收证据。前两批（FIX-1 – FIX-18）见 [`review-fixes.md`](./review-fixes.md)，
本文件**不重复**它们的结论，只记录新增部分。

## 1. 这一轮做了什么

- **四路独立只读审计**：`native`（C++ SSH 服务 + 桌面宿主）、`python`（Agent + 控制平面）、
  `desktop`（React UI + TS 契约）、`build`（门禁 / CI / 打包 / 文档）。审计者**只读**，
  不修改仓库；每条结论要求给出 `file:line` 与可达路径。
- **去重**：每条先与前两批的 18 项及 10 份原始报告逐条比对，重复的不再计。因此下面的
  26 项都是本轮**新增**的判定，而不是旧账。
- **三路修复 + 一路门禁加固**：按写作用域隔离（`native/**`、`agent/**` + 服务端、
  `apps/desktop/**` + `packages/agent-sdk/**`、`scripts/**` + CI），互不越界。
- **每条修复都带回归测试**，并做了**变异验证**：把修复逐条还原，确认新测试真的会红
  （见第 6 节）。只有「跑一次绿」不算验收。
- **权威门禁由 Lead 独立执行**，不采信修复者自报的绿（见第 5 节）。

## 2. 这一轮改变了哪些判断

前三轮把注意力放在「凭据外泄 / 越权 / 永久挂起」这类高危面上，结论是「没有找到本地无需
前置条件即可利用的路径」。这一轮认同该结论，但把镜头移到了**第二层**：

| 层次 | 前三轮覆盖 | 这一轮新增的判定 |
| --- | --- | --- |
| 安全边界 | host 密钥信任、审批票绑定、nonce 重放 | 审批卡**不显示被批准的内容**；MCP 静态头可被 CR/LF 注入；OAuth 回调 URL 校验可被 `localhost.attacker.tld` 绕过 |
| 结果正确性 | 是否可越权 | 终端把**包装进程**的退出码当成远端退出码（假成功）；审批并发下**双写**；租约/配额可被越界 |
| 失败可见性 | 是否永久挂起 | 兜底路径**漏清** admission，重试带死 runId；回复落库失败**静默**；`#` 锚点点击无反馈 |
| 门禁诚实性 | 门禁是否跑 | 门禁**只跑旧二进制**；CTest 零用例仍绿；本地无工具链时报成「编译错误」 |

一句话：前两批修的是「会不会出事」，这一批修的是「出了事你看不看得见、算不算数」。

## 3. 修复清单

### 3.1 native 宿主与 SSH 服务（FIX-19 – FIX-26）

| 修复 | 问题 | 改动 | 验收 |
| --- | --- | --- | --- |
| **FIX-19** | `terminal.closed` 上报的是 `yukinal-ssh-service` **包装进程**的退出码（实际恒为 0），远端命令失败也显示成功 | 新增私有 `terminal_exit.hpp`（`RemoteExit` / `parse_closed_frame` / `TerminalExitTracker`）；`desktop_terminal.cpp` 在 `handle_frame` 中消费服务的 `closed` 帧，按会话加锁保存；`reader_loop` 只回收进程不再读它的状态 | 帧状态机单测 7 条（整数 / 显式 null / 无帧 / 缺字段 / 字符串 / 负数）；两条端到端：worker 发 `closed exit_code=7` → UI 载荷为 7，worker 不发帧 → `null`。`closed` 缺失时**绝不**伪造 0 |
| **FIX-20** | sidecar 请求超时只丢结果，不取消 Python 侧 run，模型与工具预算继续被消耗 | `sidecar.cpp` 超时分支在释放 pending 后发**有界**（≤1 s）的 `agent.run.stop` 请求，错误全部吞掉；`agent.run.stop` 自身不再触发递归取消 | 假 sidecar 永不回答 `agent.run.start`：断言调用在 10 s 内超时，且 5 s 内收到带 `id` 且 `params.runId` 相同的 `agent.run.stop` |
| **FIX-21** | MCP 静态认证头只校验**名字**，不校验**值**；`httplib::write_headers` 原样写出，`\r\n` 可注入新头 | 新增 `header_safety.hpp` 单一谓词 `has_header_control_characters`（C0 / DEL / C1 `C2 80..9F`）；保存时用 `INVALID_PARAMS` 拒绝且**不落凭据库**；加载时再校验一遍，坏头单独丢弃并给出诊断，不拖垮整个 server | `"a\nB: c"` / `"a\rB: c"` / `"a\0b"` 全部拒绝且凭据数不变；正常 `Bearer …` 仍接受；环回 httplib 端到端断言 `X-Injected` **从未出现在线上** |
| **FIX-22** | 终端会话无上限，一次脚本化打开即可耗尽句柄与线程 | `kMaxTerminalSessions = 8`；在 `Impl::mutex` 下**先占坑后 spawn**，`std::atomic` 保证只释放一次，已关闭但未回收的会话仍占坑（churn 下上限才是真的） | 8 个可开、第 9 个带结构化消息被拒、全部关闭后第 9 个可开；RAII 覆盖每条失败路径，注册失败不泄漏子进程 |
| **FIX-23** | `desktop_ssh.cpp` 与 `sftp_transfer_backend.cpp` 各有一份 `ScopedEnvironment`，两把锁互不相识：凭证窗口内另一个线程仍能改写同一进程的 `SSH_ASKPASS_*` | 抽出 `scoped_environment.hpp`，**进程级一把互斥量覆盖整个凭证窗口**，逆序恢复、异常安全 | 2 线程 × 200 次交叉读写：加锁后不变量恒成立；不加锁时必现。代价是凭证使用期间 SSH/SFTP 并发被序列化（见 4.1） |
| **FIX-24** | `sidecar.cpp` 在 spawn 失败后仍可能把 `running` / `process_started` 置真；`stop()` 无界等待 | 只有 spawn 成功才发布状态，失败走 `abort_start()`；`stop()` 第二次等待限 5 s，超时强制落状态 | 不存在的 `launch.program` → `SidecarError` 且 `status().running == false`；`stop()` < 5 s 返回 |
| **FIX-25** | `transfer_manager` 的 permit 在手写路径上可能不归还；detached worker 的 `catch (...)` 空吞异常，任务永远停在「进行中」 | `PermitGuard` RAII 持有 permit；detached 线程捕获所有异常并 `push_failure` / `finalize_status(Failed)` | 假 backend 抛非 `TransferError` → 快照变 `Failed` 且 permit 已归还 |
| **FIX-26** | `mcp_server_save` 自写一套 URL 判断，`http://localhost.attacker.tld/` 与带 userinfo 的形式能通过，与 `validate_oauth_url()` 不一致 | 复用**同一个** `validate_oauth_url()`，保存规范化后的 URL，错误码与消息保持一致 | 环回绕过与 userinfo 用例被拒、正常环回地址通过（`test_database.cpp`） |

### 3.2 控制平面与 Agent（FIX-27 – FIX-32）

| 修复 | 问题 | 改动 | 验收 |
| --- | --- | --- | --- |
| **FIX-27** | `decide_approval` / `revoke_approval` 是「读 → 判断 → 写」，并发两票都通过，审批可被**二次裁决** | 改为 `UPDATE … WHERE state = <观察到的状态>` 的 CAS，输的一方拿到冲突而不是覆盖 | 并发双请求从 `[200, 200]` 变 `[200, 409]`；还原源码后新用例必红 |
| **FIX-28** | `claim_queued_task` 只认 task id，不校验它是**哪张审批**换来的，可用 A 的审批执行 B | 增加 `approval_id` / `tenant_id` / `kind` / `fingerprint` 参数，用 `EXISTS` 子查询把审批与任务、操作类型、输入指纹绑在一起 | 拿错票无法派发；还原后「任务被派发」直接可见 |
| **FIX-29** | `expire_stale_approvals` 先查后改，两个 worker 会对同一行各写一条审计；竞态下过期计数虚高 | 单条条件 `UPDATE … RETURNING`，只对**真的被这次更新到**的行写审计 | 并发下 `expired == 1`（还原后为 2）；审计条数与实际状态变化一一对应 |
| **FIX-30** | `create_approval` 允许把 `taskId` 挂到与 node/kind/fingerprint 不匹配的任务上 | 建票时校验绑定关系，冲突返回 409 `conflict` | 不匹配的 `taskId` 从 `201` 变 `409` |
| **FIX-31** | `_approval_respond` 先写审计事件，写失败就抛，用户的批准**丢了**且 future 永不 resolve | 审计改为 `_record_run_event`（内部吞错），随后**总是** `pending.future.set_result(decision)` | 审计存储故障下决策仍送达；还原后客户端 `TimeoutError` |
| **FIX-32** | `_admissions` 只增不减，长时间运行内存无界 | 按插入序 `popitem(last=False)` 修剪到上限 | 还原后条目数 `264 > 256`，修复后恒 ≤ 256 |

### 3.3 门禁与 CI（FIX-33 – FIX-37）

| 修复 | 问题 | 改动 | 验收 |
| --- | --- | --- | --- |
| **FIX-38** | 审批卡只说「某工具需要批准」，**不显示要执行的命令**；`server.exec` 是任意 shell，用户等于为看不见的命令签字 | `AgentEntryView.tsx` 渲染 `factsSummary` 列表 + `<details>查看具体输入</details>`（复用 `printableContent`，200 000 字符上限）+ `callId` / `inputFingerprint` 短前缀（完整值在 `title`） | 命令与 JSON 同时出现在静态渲染结果里；空 `factsSummary` / `null` input 不崩；删掉渲染新用例必红 |
| **FIX-39** | 三条失败路径（sidecar 退出、restart 记录、`start()` catch）**不清** `pendingAdmission`，重试会带死 runId，该 messageId 永久 `duplicate` | 抽出纯函数 `resetAdmissionOnFailure`，三处统一调用；同时把四处 `String(error)` 换成 `errorMessage()`，并让 `errorMessage` 对带 `issues` 的 ZodError 只摘要第一条（500 字符上限） | 纯函数单测 + 三条路径的源码切片断言；逐个删除任一处 → 对应用例红 |
| **FIX-40** | `[跳到结论](#结论)` 被渲染成按钮并调用只接受 http(s)/mailto 的 opener，异常被 `void` 丢掉：点击无反应 + unhandled rejection；标题也没有 id | 标题生成稳定去重 slug，根节点带 `data-markdown-root`；`#…` 走 `scrollIntoView`，找不到目标显示行内提示；外链失败也变可见提示而不是未处理拒绝 | 锚点与标题 id 断言；外链形状（`<button>`、无 `href`）保持原样；吞掉错误后新用例红 |
| **FIX-41** | 历史列表 `placeholderData` 只比较 `queryKey[1]`，换搜索词/筛选后继续显示上一组行，且行上的删除按钮可点 | 导出 `sameHistoryQuery` 比较完整的四段 key | 改词 / 改筛选 / 改 server / 精确匹配 / null server 五组断言 |
| **FIX-42** | 运行中把按钮设成 `disabled`，键盘焦点掉到 `body`，读屏也不再宣告；违反仓库自己在 `AgentHistoryRow.tsx` 写的规矩 | 三处改 `aria-disabled` + 早退，保留 `title` 说明原因 | 断言 0 个 `disabled`、2 个 `aria-disabled`；改回 `disabled` 必红 |
| **FIX-43** | `agent-sdk` 的 `#failAll` 清 pending 但不置 `#closed`，transport 关闭后新请求会挂满 30 s 超时 | `#failAll` 里置 `#closed = true`（与 `close()` 幂等） | transport 关闭后在途请求立即拒绝，后续 `ping` < 1 s 报 `agent client is closed` |
| **FIX-44** | Agent 回复落库失败被静默吞掉：运行中删掉当前对话，回答只剩内存，刷新后永久消失 | `recordAssistantMessage` 返回布尔，失败时由 `useAgentRun` 追加系统提示（`ASSISTANT_REPLY_NOT_SAVED`） | 纯函数单测 + 去掉 false→提示 的映射后必红 |

### 3.4 桌面 UI 与 TS 契约（FIX-38 – FIX-44）

| 修复 | 问题 | 改动 | 验收 |
| --- | --- | --- | --- |
| **FIX-33** | CTest 跑的是**磁盘上已有的**二进制：改了源码但没重建，门禁照样绿；CTest 找到 0 个用例也退出 0 | 新增 `build native test targets` 步骤，排在 CTest 之前；CTest 加 `--no-tests=error`；新增 `scripts/lib/gate.test.mjs` 把步骤顺序与参数钉死 | 5 条护栏（显式坏构建目录必须 exit 2、步骤顺序、精确 argv、空 CTest 退出码、输出捕获） |
| **FIX-34** | 本机 WebView2 SDK 1.0.1150.38 < 要求的 1.0.2903.40，全量构建必然失败 → 门禁永久红；但直接跳过 native 构建又重开 FIX-33 的洞 | 先跑**全量**构建（fail-closed）；仅当输出归因于 WebView2 时，剔除 GUI 目标后重试其余目标；成功也打印**醒目告警**（两个版本号、原始报错、`desktop_host_self_check` 跑的是旧二进制、本地覆盖不完整） | 决策表单测；假 runner 覆盖「WebView2 重试 / 无关错误不重试 / 成功不重试」；实测本机绿且告警完整 |
| **FIX-35** | 新构建步骤要求 `nmake`/`cl` 在 PATH：普通 PowerShell 里直接 `pnpm check` 会失败，且 `fix:` 文案把「工具链不在 PATH」说成「编译错误」 | Windows 上探测生成器对应的构建工具；不在 PATH 时用 `vswhere` 找 `vcvars64.bat` 并以 `cmd /d /s /c "…"` 加载环境；找不到工具链时给出**准确**提示且不尝试构建；`fix:` 文案按失败类型分流 | 13 条护栏：工具在 PATH → argv 不变；工具缺失 + vcvars → `cmd` 形式并透传 `windowsVerbatimArguments`；两者都没有 → 精确失败且**零次**构建；NMake 缺失与真实编译错误给出不同提示 |
| **FIX-36** | CI 工作流用默认 `GITHUB_TOKEN` 权限、checkout 持久化凭据；pip 缓存只覆盖一个项目 | `permissions: contents: read`；`persist-credentials: false`；`cache-dependency-path` 同时覆盖 `agent/pyproject.toml` 与 `services/control-plane/pyproject.toml` | 工作流文件断言 + 门禁绿 |
| **FIX-37** | shared 的 Agent 版本 fixture 写死 `1.0.0`，与 `agent/pyproject.toml` 的 `0.1.0` 漂移，测试只验证「彼此相等」而不是「与真实来源相等」 | 从 `agent/pyproject.toml` 解析版本再比对；三个 IPC fixture 同步为 `0.1.0`；`core_ping.version` 仍跟 `APP_VERSION` | shared 142 全绿；把 fixture 改回 `1.0.0` 必红 |

## 4. 关键设计取舍

### 4.1 凭证环境窗口改为**全进程串行**

`SSH_ASKPASS_*` 是进程级环境变量，而「设环境 → 起子进程 → 恢复」是一个不可分割的窗口。
两份各自加锁的 `ScopedEnvironment` 只保护自己的调用者，跨模块就会互相踩。正确做法只有两种：
把凭证**不经过环境变量**传递（改协议），或让窗口**全进程串行**。这一轮选后者：改动小、
语义可证，代价是凭证使用期间 SSH/SFTP 并发下降。**任何将来新增的 secret-env 路径必须用同一个
头文件**，否则这层保护会重新裂开。

### 4.2 审批并发用 CAS，而不是「先查后写」

控制平面的问题是典型的 lost-update：两次裁决都看到 `pending`，都写成功。CAS 把「我看到的
状态」变成写入的前置条件，输的一方收到冲突。**残留风险**：本地只用 SQLite 验证，
PostgreSQL 下依赖 `READ COMMITTED` 的重新求值语义 —— 这是标准行为，但没有在本机实测。

### 4.3 `_approval_respond`：审计持久性换取活性

原实现把「写审计」放在「兑现 future」之前，审计写失败 ⇒ 用户点了批准却什么都没发生。
现在审计是尽力而为，**决策一定送达**。这是明确的方向选择：审批是用户意图，不能因为记账
失败而丢失；代价是极端情况下少一条审计事件（后续可由 run 事件补齐）。

### 4.4 审批卡展示 `input` 不新增泄密面

`input` 在服务端 `model_agent.py` 已按敏感 key 脱敏并截断，host 只剥掉内部的 `hostInput`。
也就是说：**UI 拿到的本来就是给模型看的、已脱敏的那一份**。不显示它，用户就只能为一条看不
见的命令签字 —— 那才是真正的不安全。

### 4.5 `terminal.closed` 的 `null` 是诚实值，不是缺陷

远端退出码有三种情况：拿到整数、服务显式发 `null`、服务在发帧前就死了。后两者都序列化为
`null`，因为**「不知道」和「0」必须区分**。UI 必须把 `null` 渲染成「未知」，不能渲染成成功。
这一条比「总是给个数字」更重要：假成功会让人以为备份成功了。

### 4.6 门禁的局部成功必须**自己说出来**

WebView2 回退让本机能绿，但那条绿是**不完整**的。所以回退成功时打印四条告警，并且行尾写
`(desktop host skipped: WebView2 SDK too old)`。判据是：**一个诚实的黄比一个沉默的绿更有价值。**

### 4.7 `cmd /s /c` 必须传 `windowsVerbatimArguments`

实现回退时踩到的真实坑：Node 在 Windows 上会按 MSVCRT 规则重新加引号，`call "C:\Program
Files\…\vcvars64.bat"` 里的引号被转义成 `\"`，cmd 解析失败后**静默 exit 1 且无任何输出** ——
看起来和「编译器崩了」一模一样。实测三种写法（`/c`、`/d /s /c` 普通参数、`/d /s /c` +
`windowsVerbatimArguments`）只有第三种工作，因此 `runNativeBuildStep` 现在会把
`windowsVerbatimArguments` 透传给 spawn，并有专门的护栏用例钉住它。

## 5. 门禁与测试（修复前后）

权威门禁由 Lead 在**普通 PowerShell（未加载 vcvars）**下独立执行：

```
$ node scripts/check.mjs
…
! native: desktop host target was NOT compiled locally (WebView2 SDK 1.0.1150.38 < required 1.0.2903.40).
! native: local native coverage is NOT full; CI builds the full set.
✓ build native test targets (desktop host skipped: WebView2 SDK too old)
…
check: green
```

| 项目 | 前两批完成时 | 本轮之后 |
| --- | --- | --- |
| 门禁步骤 | 14 | **15**（新增 native 构建 + 3 条护栏步骤） |
| 门禁护栏测试 | 2 | **13** |
| CTest 目标 | 13 | 13（`yukinal_ssh_tests` 用例 64 → **86**） |
| 控制平面 | 149 | **157** |
| Python Agent | 167（skip 4） | **173**（skip 4） |
| `packages/shared` | 142 | 142 |
| `packages/provider-sdk` | 4 | 4 |
| `packages/agent-sdk` | 5 | **6** |
| `apps/desktop` | 325（1 skip） | **344**（343 pass + 1 skip） |
| 文档链接 | 39 文件 / 207 链接 | **41 文件 / 214 链接** |

单独复核（Lead 直接跑，不经队友转述）：

```
$ node --test scripts/lib/gate.test.mjs
ℹ tests 13   ℹ pass 13   ℹ fail 0

$ pnpm -r --if-present typecheck        → 4/4 Done
$ pnpm --filter @yukinal/desktop test   → 344 tests, 343 pass, 1 skip
$ ctest --test-dir native/build-nmake -C Release --output-on-failure --no-tests=error
                                        → 100% tests passed out of 13
$ git diff --check                      → exit 0
```

## 6. 变异验证（反向证明）

每条修复都做了「把修复还原 / 削弱，确认新测试会红」的验证。**没有反证的修复不算交付。**

| 修复 | 变异 | 结果 |
| --- | --- | --- |
| FIX-27 | 还原 CAS 为「读后写」 | 双裁决从 `[200, 409]` 回到 `[200, 200]` |
| FIX-28 | 去掉审批 `EXISTS` 绑定 | 拿错票的任务仍被派发 |
| FIX-29 | 还原为「先查后改」 | 并发过期数 `1 → 2` |
| FIX-30 | 去掉 `taskId` 绑定校验 | `409 → 201` |
| FIX-31 | 审计失败时先抛 | 客户端 `TimeoutError` |
| FIX-32 | 去掉修剪 | 条目 `264 > 256` |
| FIX-38 | 删掉 `factsSummary` / `input` 渲染 | 审批卡断言红 |
| FIX-39 | 三条路径各删一次 `resetAdmissionOnFailure` | 对应路径断言红（3/3） |
| FIX-40 | 去掉标题 id / 吞掉链接错误 | 锚点用例红 |
| FIX-41 | 谓词退回只比 `serverId` | 换词 / 换筛选断言红 |
| FIX-42 | 改回 `disabled` | `aria-disabled` 断言红 |
| FIX-43 | 去掉 `#closed = true` | 「关闭后请求挂起」用例红 |
| FIX-44 | 去掉 false→提示 映射 | 落库失败提示用例红 |
| FIX-19 | 不消费 `closed` 帧 | `exitCode` 断言红（伪造 0 / null 不符） |
| FIX-21 | 只校验名字 | 注入用例红 |
| FIX-23 | 去掉进程级互斥量 | 2 线程交叉断言红 |
| FIX-25 | 保留空 `catch (...)` | 快照停在「进行中」 |
| FIX-33 | 删掉构建步骤 / 去掉 `--no-tests=error` | 护栏用例红（步骤顺序、空 CTest 退出码） |
| FIX-34 | 去掉 WebView2 判定 | 无关编译错误也会被重试 |
| FIX-35 | 去掉 `windowsVerbatimArguments` 透传 | `cmd` 形式静默 exit 1，端到端用例红 |

## 7. 仍然未修 / 明确不做

**不要**把「门禁绿」读成「没有缺陷」。以下是本轮**确认但仍未修**的项，按原因分组。

### 7.1 需要改协议或换架构，不适合塞进一轮修复

- **`agent_run_start` 最长 15 分钟的同步 WebView 阻塞**：本轮只让 sidecar 请求超时后**取消
  run**（FIX-20），WebView 侧仍是同步等待，超时后界面无「正在取消」反馈。根治要把该命令改成
  「受理即返回 + 事件推进」。
- **`ConversationStore` 在事件循环上同步执行 sqlite3**：单次毫秒级，但长事务会卡住整个 sidecar
  的帧处理。需要搬到线程池或改异步驱动。
- **`serve_stdio` 每帧 `create_task` 无上限**：恶意/异常客户端可以瞬间堆积任务。需要每连接
  信号量或帧队列。
- **`_provider_list` 串行发起最多 8 s 的凭据 RPC**：多 provider 时线性叠加。需要并发 + 总超时。
- **`terminal.closed` 的「未知」在 UI 侧仍只显示「会话已关闭」**：`exitCode: null` 目前不带
  「状态未知」文案（`TerminalPane.tsx`）。这是 FIX-19 的**下游**缺口，本轮未改前端。

### 7.2 供应链与 CI 硬化

- **FetchContent 用可变 tag**：`nlohmann v3.11.3`、`cpp-httplib v0.18.3`。可固定的提交已核出
  （`9cca280a4d0ccf0c08f47a99aa71d1b0e52f8d03`、`a7bc00e3307fecdb4d67545e93be7b88cfb1e186`），
  但改 pin 需要重新验证整条 native 构建链，本轮未做。
- **Python CI 依赖未锁**：`pip install -e "…[agent,models]"` 的上界靠 `pyproject.toml`，CI 没有
  requirements lock 或 hash 校验。
- **`pnpm-workspace.yaml` 有 `minimumReleaseAgeExclude` 但没有 `minimumReleaseAge`**：排除项
  因此没有实际约束对象，读起来像已经开了「新版本冷静期」。

### 7.3 本轮审计发现但判定为「低价值 / 需先确认可达性」

- **context-epoch effect 清 transcript 但不置 `running = false`**（`AgentPanel.tsx`）：当前唯一
  调用点被 `busy` 挡住，属「一个 store action 就能踩到」的隐患。**hypothesis**，未修。
- **`TimestampSchema` 只校验长度不校验日期**：`Date.parse` 得 `NaN` 会让「过期」判断恒为
  false（服务端仍会拒，故非越权）。
- **`AgentComposer` 的 listbox 缺 `role="combobox"` / `aria-expanded` / `aria-activedescendant`**：
  读屏无法跟随高亮项。
- **`MemoryLibrary` 用 `window.confirm`**：阻塞式弹窗在 WebView2 里会冻结动画与事件，与仓库
  其他处的内联确认条不一致。
- **2.3 MB 的 Nerd 字体被无条件打包**，而默认终端字体并不是它。
- **`LogsPane` 与 `AgentFeed` 无虚拟化**：30 s 轮询整表重建 / 500 条上限内全量渲染。
- **`packages/shared/src/schemas/agent.ts` 的时间戳不校验格式**、**`AgentEntryView` 空文本
  渲染省略号**：均属低风险展示问题。

### 7.4 验证能力边界（不是没修，是没法在本机证明）

- **PostgreSQL 未实测**：CAS 依赖 `READ COMMITTED` 的重新求值，本地只有 SQLite。
- **无端到端 PTY keyboard-interactive 测试**：要一台会发挑战的 SSH 服务器；已覆盖状态机与
  载荷一致性，未覆盖「字节真的写进 stdin」。
- **Linux `SIGPIPE` 只到源码级护栏**：本机无 Linux，行为级未闭环。
- **WebView2 GUI 目标在本机无法编译**：SDK 版本不足，故 `desktop_host_self_check` 跑的是
  上一次构建的二进制；CI 才构建全集。
- **桌面 UI 无 DOM 测试环境**（41 个测试文件全是 `renderToStaticMarkup`）：hook 内的状态机
  只能靠「纯函数 + 源码切片断言」钉住，不是真正的渲染级验证。

## 8. 复核方式

```bash
# 1. 权威门禁（普通 shell 即可，工具链由门禁自己发现）
node scripts/check.mjs

# 2. 门禁自身的护栏
node --test scripts/lib/gate.test.mjs

# 3. 分语言单独跑
ctest --test-dir native/build-nmake -C Release --output-on-failure --no-tests=error
python -m unittest discover -s services/control-plane/tests -p "test_*.py"
python -m unittest discover -s agent/tests -p "test_*.py"
pnpm -r --if-present typecheck
pnpm -r --if-present test

# 4. 空白字符/换行污染
git diff --check
```

**一个曾经的真实教训**：本轮的 native 构建步骤在普通 shell 下先失败过一次，原因是
`nmake` 不在 PATH；修复过程中又发现 `cmd /s /c` 必须配 `windowsVerbatimArguments`，否则
cmd 静默 exit 1。两次都不是源码问题，但**都表现为「编译错误」** —— 这也是为什么门禁现在会
区分「工具链缺失」与「编译器报错」两种 `fix:` 文案。
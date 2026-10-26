# 变更记录

## 2.0.1 — 2026-10-27

- 项目负责人确认 Agent 能力验收合格，同意正式发布。该确认与自动化测试、模拟 UI 审计分别记录，见[发行说明](./releases/2.0.1.md)。
- 文件、终端、日志、服务四按钮直接切换主界面，移除额外标签栏、新增标签、分栏、展开和返回对话操作；Agent 对话留在浮栏中。
- 切换工具保留终端、目录、日志筛选与草稿；切换服务器时当前工具跟随目标，保留旧终端。紧凑模式打开、关闭参考信息后恢复原工具。
- Token 展开不再改变状态行位置；统一浮栏控件高度与对齐，修复模型、推理与设置弹层裁切和窄屏输入区遮挡。
- 操作记录按当前服务器过滤，修复含自然语言斜杠的模型名称截断；看板在窄区域改为可读的网格布局。
- 合入此前 Agent 工具循环、值守与第五轮审计收尾：巡检保留有效基线、顺序调用 SSH 工具，失败原因可读；明确不用工具时不再续跑或派发任务。工具能力与审批策略沿用已验收实现。
- 统一未知退出状态、超时、取消的呈现，补齐消息输入与破坏性操作的可访问性，清理旧角色路由与 UI 推断阶段。详见[第五轮审计](./audit-round5.md)。
- 重写发行、工作区交互和文档入口；应用版本统一为 `2.0.1`，Python Agent 包版本继续独立为 `0.1.0`。
- 将 Vite 更新至 `8.2.4`、Zod 更新至 `4.6.6`，修复依赖安全审计告警，同步锁文件。

## 2.0.0-preview.2 — 2026-10-20

- 连接后自动采集服务器资源，增加 NVIDIA GPU 和基础安全信息；只读探针共用可扩展目录，慢项限时、失败隔离，Docker 避免重复探测。详见 [服务器检测工具包](./server-toolkit.md)。
- 修复 OAuth 回调监听器并发关闭时可能触发的原生断言崩溃。
- goal 运行取消默认调用和轮数配额；持续目标加入阶段验收、参数修正和无进展检查，保留取消及总预算。
- 独立只读工具并行、推理参数选择、真实请求用量与耗时，以及按压力投影历史上下文。
- 精简公开回复、折叠成功操作、保留可解析的命令结果，改进进展、审批与轮次导航展示。
- 加入本地二维和三维信息绘图协议。具体范围和验收边界见 [AI 运行与对话体验](./ai-runtime-performance.md)。
- 程序与安装包使用 SemVer 预览版本 `2.0.0-preview.2`。

本文件记录对用户有意义的版本变化和交付状态。详细实现与历史讨论可通过 Git 记录追溯。

## 2.0.0 — 2026-10-20

重写后首个正式发行目标：采用 C++ WebView 桌面宿主、独立结构化 SSH 服务、Python Agent 和 React 界面，并提供 Windows NSIS、macOS DMG 与 Debian Linux 安装包。此提交的全平台门禁与安装器 smoke 正在 GitHub Actions 验证；安装包未签名，macOS 未公证，真实 Provider/MCP/用户 SSH 主机与安装后 GUI 文件传输仍需实际环境验收。发布资产应以对应的成功 workflow 为准。

## 2.0.0-rc.7 — 2026-10-20

新增 Python Agent 的显式 opt-in live Provider 工具循环探针与本机 OpenAI-compatible stub 测试；远程自定义 endpoint 限定为 HTTPS，loopback stub 可免 API key。离线探针回归 6/6 通过。GitHub Actions [Run 39319681035](https://github.com/Bad0RANG3/Yukinal/actions/runs/39319681035) 的 Windows NSIS、Ubuntu DEB、macOS DMG 构建、完整 gate 与安装 smoke 全部通过；Ubuntu CTest 9/9，含 OpenSSH 容器集成。该候选结果不代表真实 MCP/SSH 目标任务、A/O/F/R 全路径或安装版三平台 GUI 验收已经完成。

## 2.0.0-rc.6 — 2026-10-20

修复 Windows 运行发布门禁时的 symlink 用例误报：Windows 文件权限模型不提供 POSIX 可执行位，测试现在只在 POSIX 系统验证该位；macOS 仍会验证 Python.framework 文件链接展开时保留可执行权限。GitHub Actions [Run 39309839659](https://github.com/Bad0RANG3/Yukinal/actions/runs/39309839659) 的第 2 次 attempt 全绿：Windows NSIS、Ubuntu DEB、macOS DMG 均完成安装/启动/卸载 smoke，并验证独立用户数据保留。macOS 首次 job 在 hdiutil 创建 DMG 时失败，同一提交重跑通过；该结果确认 runner 的重试通过，不代表真实 Provider、OpenSSH、GUI 拖放和签名/公证已验收。

## 2.0.0-rc.5 — 2026-10-20

修复 macOS Agent 冻结载荷暂存：RC.3 的 macOS job 发现 PyInstaller 会在 Python.framework 中产生内部 symlink，而发布 manifest 只接受普通文件。构建现在只展开目标仍在 Agent runtime 内的链接，并拒绝越界链接、目录循环和特殊文件；新增覆盖展开和越界拒绝的测试。修复 Windows NSIS 构建参数把路径双重引号传入 `OutFile` 导致的安装器编译失败。等待 RC.5 三平台完整验证。

## 2.0.0-rc.4 — 2026-10-20

修复 macOS/Linux 候选门禁中发现的 SSH 终端子进程生命周期问题：远端 shell 关闭后，桌面宿主收到 `closed` 帧会关闭工作进程的 stdin，确保输入读取线程退出并回收子进程。终端宿主集成 mock 现在会等待 stdin EOF，以覆盖此行为。rc.4 等待三平台全门禁和安装器 smoke 结果。

## 2.0.0-rc.3 — 2026-10-20

发布候选工作流现在会在三个操作系统上分别创建并填充 `agent/.venv`，保证全量门禁与 PyInstaller 构建使用同一安装好的 Python 环境。此前 Windows 干净 runner 完成门禁后，在冻结 Agent 构建时因该虚拟环境不存在而退出；macOS/Linux 的 Windows 路径模拟测试也已在 rc.2 修复。rc.3 等待新的三平台安装器 smoke 结果。

## 2.0.0-rc.2 — 2026-10-20

修复首轮跨平台 CI 暴露的问题：macOS/Linux 跑门禁时，对 Windows 工具链的模拟测试错误地使用 runner 本机的 POSIX 路径拼接，导致三个 guardrail 测试失败。路径解析现在按目标平台选择；Windows 本地完整门禁及对应 guardrail 测试通过。新一轮三平台安装器 CI 正在运行。

## 2.0.0-rc.1 — 2026-10-20

这是重写后首个跨平台安装包候选，尚不是稳定版。除桌面工作区标签导航、Agent 会话恢复、SSH 认证挑战、流式传输和控制平面改进外，本候选增加 Windows NSIS、macOS DMG、Debian Linux 安装包构建与安装/启动/卸载 smoke。自动 smoke 不替代真实 Provider、SSH、GUI 文件拖放、签名和公证验收；发布状态见[交付与发布](./release.md)。

- 2026-10-19 **桌面端工作页改为顶部浏览器式标签行**：文件／终端／日志／服务的打开入口从 `AgentHeader` 右侧控制区移到 `AgentHeader` 之下的独立一行（`apps/desktop/src/features/workpages/WorkspaceTabStrip.tsx`），四个启动按钮与已打开的工作页标签并排；标签从 `WorkpagesDock` 内部搬到了这一行，内容舞台不再被标签顶动，对话区始终留在下方。标签横向滚动时启动按钮不跟着滚走，窄容器下只折叠按钮文字（1000px）、提示文案（780px）与服务器名（560px），入口本身始终可见。启动按钮并入标签行后不再单独保留 `WorkpagesLauncher`，`agent-history.css` 中针对它的折叠规则随之迁到 `apps/desktop/src/styles/workpages-strip.css`。

  只做视图导航，不涉及执行语义：没有"自动批准""替我继续"一类入口，也没有按标签的进度模型（不存在这层数据，造出来就是伪造）；审批仍然只能在对话里的审批卡上给出，权限模式与运行模式不变，运行中切换服务器依旧被拒绝，关闭标签只切可见性、不改每页打开时捕获的 `serverId`，终端会话照旧保留。被关闭而仍挂载的终端残留帧不再带 `role="tabpanel"`／`aria-labelledby`（标签已不存在，保留配对会让读屏的标签/面板关系断掉），焦点在关闭后回退到后继标签、无后继时回退到启动按钮。

  另新增一个已持久化但尚未暴露入口的方向轴 `workpagesOrientation`（`horizontal`/`vertical`，默认 `horizontal`，落在 `preferences-store.ts` 并进 `partialize` 与 `sanitizePreferences` 白名单）：同一个值同时决定标签行排布方向与方向键轴向（`apps/desktop/src/features/workpages/tab-navigation.ts`），v1 只渲染横向以便日后加设置项时无需再改存储结构。**未做**满宽 40px 原生标题行（需同步升级 `window-shell.ts` 的 refine、`windows_window_frame.cpp` 的校验与 `paint_caption_surface` 的绘制分支，属于原生侧工作）、垂直标签的切换 UI，以及"更多标签"溢出菜单。**未做真实桌面宿主与真实供体的实机验收**，原生对照（拖动、双击最大化、≥1181px 融合路径与非融合降级路径）仍需单独验收。验证：`pnpm check` 绿；`apps/desktop` 333 项测试 332 通过 1 跳过；另见[工作页标签栏设计说明](./workspace-tabs-spec.md)。

- 2026-10-19 **`server.exec` 命令黑名单**：C++ SSH 服务配置新增 `server_exec_denylist`，可写在根级（服务级，所有 target 继承）和每个 `targets[i]` 内（在该 target 自己的条目之前追加，只能增不能减，target 未写该键也照样继承根级名单）。条目可以是裸字符串（区分大小写的字面量）或 `{"pattern","regex","case_insensitive"}` 对象；最多 256 条，单条模式不超过 1024 UTF-8 字节。匹配前对命令和字面量做同一套空白归一化（连续空白折成一个空格再去首尾），字面量按子串匹配，`regex` 条目按 ECMAScript `regex_search`（`std::regex::optimize`，`case_insensitive` 时加 `std::regex::icase`）。名单只在两处程序代码里生效，且都只减不增：C++ SSH 服务在 `server_exec_operation` 里、在原有 `allow_server_exec` 一次性能力门之后、在任何命令拼接与 SSH 连接之前拒绝；C++ 桌面宿主改用 `YUKINAL_SSH_SERVICE_CONFIG` 指向文件里的根级名单，在**消耗一次性批准票据之前**拒绝，并在非空时把名单盖章进临时单目标配置。拒绝错误码统一为 `denied_by_policy`（HTTP 403），报文不回显命令或被命中的模式；配置解析期即校验，名单畸形（类型错、超过 256 条、空模式、超长模式、非布尔标志、未知键、正则编译失败）会让服务启动失败而不是带着空名单运行。该能力只约束 Agent 的 `server.exec`，不影响人在自己终端里手敲命令，也不替代审批流程。**它是纵深防御护栏而不是沙箱**：匹配只发生在原始命令字符串上、不做 shell 展开，`r''m`、`$IFS`、base64 管道等混淆写法可以绕过。两处有意的不对称：桌面宿主只读该文件的**根级** `server_exec_denylist`（忽略同一文件里 `targets[*]` 下的名单，后者只作用于服务侧）；且打包清单的启动段（`scripts/package.mjs:467`）只转发 `YUKINAL_AGENT_ENTRY`/`YUKINAL_SSH_SERVICE_BINARY`/`YUKINAL_DATA_DIR`，因此标准安装包启动时不会转发 `YUKINAL_SSH_SERVICE_CONFIG`——此时宿主侧名单为空、只有服务侧名单生效，而显式导出该变量的开发/CLI 启动两侧都生效。

- **文档与仓库卫生整理（三轮审计）**：第一轮清理仓库根部误留的构建/验证 `.txt` 临时记录（`build*`/`ctest*`/`pnpm*`/`pub*`/`push*`/`commit*`/`test_out.txt`），并在 `.gitignore` 中按根目录锚定忽略，避免再次误提交。第二轮修正仍指向已删除 Rust/Tauri 宿主与 legacy Node 回退的过期文档（`CONTRIBUTING.md`、`SECURITY.md`、`agent/README.md`），删除 `.editorconfig` 中已无对应源码的 `[*.rs]` 段。第三轮合并 `package.json` 中重复声明的 `smoke:*` 脚本（同一命令原为两条键），并把 `docs/README.md` 索引补上 `native-desktop-host.md` 与 `cpp-desktop-handoff.md`。新增英文 [README](../README.md) 与中文 [README.zh-CN.md](../README.zh-CN.md)，顶部互相跳转，含徽章、架构图、组件表、功能状态表以及构建/配置/错误/安全速查。源码与接口版本未变。

- **控制平面按租户策略**：审批要求与有效期不再是全服务单值，而是每个租户自己的策略，用的正是共享风险层的词汇（`packages/shared/src/types/risk.ts`）。控制平面把两个问题分开：kind **是什么**（风险）与**发生什么**（该权限档的模式）。风险来源按优先级是宿主对自己内建工具的自己声明、本租户的声明、闭集默认 `high`；宿主的两组内建工具被逐一镜像——计划绑定的只读观察用 `read`，按名点名的状态变更工具（`server.exec`/`docker.restart`/`systemd.restart`/`package.install`/`filesystem.write`/`filesystem.edit`/`filesystem.backup`/`filesystem.backup.cleanup`/`filesystem.restore`）恒为 `high`。风险收敛为权限档（`read`/`write`/`dangerous`），租户的**档位模式**决定结果：`auto` 无需决定即运行、`ask` 需已批准的决定、`deny` 直接拒绝（创建与派发都拒）。`GET/PUT/DELETE /v1/tenants/{id}/policy` 读/写/清除策略（未存时回显 `stored:false` 与 production 默认），`PUT` 还带 `approvalTtlSeconds`（60–86400），即该租户决定的窗宽；给 `environment` 会按内建表推导档位（`local`/`production` 对写要审批，`development`/`staging` 放行，未知环境等同 production，绝不更宽），显式传 `tierModes` 则不改变环境语义而偏离该表。`GET /v1/tenants/{id}/tool-policies` 列出控制平面能点名的每个 kind 及其风险/档位/模式/来源（`host`/`declared`/`default`），`PUT .../tool-policies/{kind}` 声明、`DELETE` 清除——声明即租户对控制平面未随附的 `mcp.*` 工具的信任表态。两件事不可被声明掉：宿主点名的不变量在**决定函数本身**先于任何声明生效（而不只是在拒绝入库的端点里），`dangerous` 档永不为 `auto`——因此无论以何种方式写入，都不存在让 `server.exec` 无决定运行的配置。档位模式按解析后的值存储而非读取时推导，日后改动内建默认不会静默放开已做选择的租户。策略变更会作用于**已排队的任务**：收紧为 `ask` 会挡住此前豁免、未记录授权的排队任务，`deny` 会停住已获授权并已入队的任务（`403 action_denied`，因拒绝无法被任何决定清除），通道以 `waiting.reason=policy_denied` 上报。新增迁移建 `tenant_policies`/`tool_policies` 两表。控制平面测试 91 → 129，并新增真 uvicorn 端到端：未声明的 `mcp.*` 被闭集默认拒绝→声明 `read` 后无需决定即运行→重声明 `server.exec` 为 409→租户 TTL 给下一次决定 120 秒窗→`dangerous:auto` 为 422→`deny` 既拒不新建的 `server.exec` 又停住已授权的那一单并以 `policy_denied` 上报。

- **控制平面审批生命周期**：批准的审批现在带**有效期与使用预算**——一个永不过期、可无限次兑现的"审批"不是对某个动作的决定。批准时写入 `expires_at`（决定时刻 + 900 秒）与 `max_uses`（请求 `maxUses` 默认 1、上限 100；决定方只能收紧不能放宽）。三种方式失效：**到期**（每个检查都比时钟，未清扫的行照样拒绝；清扫只让存储状态说真话——列表清扫本租户，`POST .../approvals/expire` 供运维/cron 显式驱动且幂等并返回移动条数）、**撤销**（`POST .../approvals/{id}/revoke`，pending 或 approved 皆可；撤销不重写历史，任务保留其授权记录、只是拒绝启动）、**用尽**（建任务时以 `WHERE uses < max_uses ... RETURNING` 原子扣减）。扣减**不在派发时重复检查**（否则一次性审批会卡住它自己授权的那一个任务），派发只检查该决定仍为 `approved` 且未过期；任务新增 `approval_id` 记住是谁授权的，账本可答"哪个决定为哪个任务买单"。通道 `waiting` 的原因据此细化为 `approval_expired`/`approval_revoked` 等。另修一处一致性缺陷：所有响应时间戳统一序列化为显式 UTC（`...Z`）——SQLite 无时区类型、会返回裸 wall clock，否则同一字段读库与回显会不一致、客户端无法直接与时钟比较。新增迁移给 `approvals` 加 `expires_at`/`uses`/`max_uses`/`revoked_by`/`revoked_at`、给 `tasks` 加 `approval_id`。控制平面测试 68 → 91，并新增真 uvicorn 端到端验证：单次授权建一单后即用尽→再建被拒→已授权任务仍可启动、回填过期的决定被扫为 `expired` 并以 `waiting.reason=approval_expired` 挡住队列、撤销以 `approval_revoked` 挡、`maxUses` 由 3 收到 1 只放一单、超过上限的 `maxUses` 为 422。

- **控制平面审批强制绑定执行 + 原子派发**：审批请求改为携带 `action`/`nodeId`/`payload`，**指纹由服务端计算**（规范化 `{kind,nodeId,payload}` 的 sha256，排序键、无空白），调用方无法自报指纹；决定只对**匹配指纹**生效，因此批准“A 节点重启 nginx”不能派发“B 节点 rm -rf”，换载荷/换节点/换动作/换租户均不放行，待批与驳回同样不放行。只读观测类 kind（对齐宿主 plan-bound observation 名单：`server.info`/`server.logs`/`docker.ps`/`docker.inspect`/`filesystem.read`/`systemd.inspect`/`package.inspect` 等）免审批，**未知 kind 与所有 `mcp.*` 一律需要审批**（fail closed）。门禁查两次：建任务时（队列即“可派发”，否则会占住节点唯一 active 槽）与 `queued → running` 边（覆盖审批中途失效），拒绝为 `409 approval_required` 且报文给出应批准的指纹。派发改为**条件 UPDATE**（`WHERE status='queued'`）原子领取，两个 worker 不会把同一任务派两次；未获批的任务在通道 `ready`/`synced` 的 `waiting` 里带原因与指纹上报，不再看起来像空闲节点。新增迁移把 `approvals` 绑定到 `node_id` 与 `payload`。控制平面测试 45 → 68，并新增真 uvicorn 端到端验证：409（含指纹）→ 批准 → 派发完成 → 换载荷被拒 → 审批撤销后以 `waiting` 上报且任务留在 `queued`。

- **控制平面补齐节点身份与节点通道**：节点注册时提供 Ed25519 公钥（接受 base64/hex/`ssh-ed25519` 行，统一存规范 base64），之后每个 `/v1/node/...` 请求对 `timestamp\nnonce\nMETHOD\npath[?query]\nsha256(body)` 签名；时间窗 300 秒，nonce 落库 `node_nonces` 保证一次性（重放 401），未知节点/过期/签名错误/重放**统一 401**不泄露节点是否存在。节点路由不带 tenant id，租户从节点行推导，且节点只能操作自己的任务（同租户内也不行）。`WS /v1/node/channel` 是节点主动连出的通道：连上即把该节点 queued 任务置 running 并下发，节点回报 `status`；socket 中断把 running 任务置 `unknown`（`node_disconnected`，优雅 `close` 为 `node_closed`）且**不自动重放**，重连后可回报真实结果；断连标记在取消屏蔽作用域内执行，服务关闭时不会被静默跳过。新增迁移 `node_nonces`。控制平面测试 19 → 45，并新增真 uvicorn + 真 WebSocket 客户端的端到端签名/通道验证；依赖新增 `cryptography`（Ed25519）与 `websockets`（uvicorn 服务 WS）。

- **新增控制平面模块 `services/control-plane/`**（第四阶段，独立于桌面宿主）：FastAPI + SQLAlchemy 2.0(async) + Alembic 的多租户/节点/任务/模型配置/审批/审计服务；生产库为 PostgreSQL（`asyncpg`），测试与本地开发用 SQLite（`aiosqlite`）。缺 token 拒绝启动、启动时校验 schema 已迁移、`/v1` 全部要求 bearer token。实现租户隔离（跨租户 404 不泄露存在性）、任务状态机、每节点一个 active 任务的**偏唯一索引**（数据库级并发控制）、节点重启把 running 任务置为 `unknown`（不自动重放，`recoverable` 只读列表）、凭据只存引用（模型配置禁止未知字段，任务 payload 递归拒绝 secret 形状字段）与 `/v1/metrics` 汇总。19 项测试覆盖 API/schema/迁移一致性，并实际启动 uvicorn 验证 healthz/401/建租户/metrics；`pnpm check` 新增该步骤（`services/control-plane/.venv` 缺失时打印明确 skip 原因）。PostgreSQL 部署本身、节点 HTTPS/WSS 通道、多 worker 并发与 Prometheus 指标仍未做。

- **C++ 宿主 `host.tool.execute` 完成 MCP 门禁对齐**：`mcp.*` 与非 MCP 只读工具共用 durable plan/task guardrail/目标匹配/幂等账本门禁，MCP 风险由宿主按 running+reviewed catalog 解析（无法解析即认为 effectful、fail-closed），不再存在 MCP 分支绕过 durable-plan 门禁的差异。`host.tool.execute` 的 `mcp.*` 分支同时改用与非 MCP 分支一致的 `{status, error:{code, message, retryable}}` 响应信封。

- **C++ 宿主新增分发布局与完整性清单**：`pnpm package`（`scripts/package.mjs`）把 Release C++ 宿主、冻结 Python Agent 与 C++ SSH 服务暂存成 `target/release/bundle/yukinal-<version>-<platform>-<arch>/`，写出 `SHA256SUMS.txt` 与 `release-manifest.json`（含启动契约 `YUKINAL_AGENT_ENTRY`/`YUKINAL_SSH_SERVICE_BINARY`），`pnpm release:manifest` 重建清单、`pnpm package -- --verify` 逐字节复核并在不一致时非零退出。仍无安装器（NSIS/MSI/DMG/deb）、签名与三平台真机验收。

- **删除整个 Rust/Tauri 桌面宿主（按用户要求）**：移除 `crates/`、`apps/desktop/src-tauri/`、`Cargo.toml`/`Cargo.lock`/`rust-toolchain.toml`/`.cargo/`，以及已退休的 legacy Node agent（`apps/agent/`）和只能由它运行的 smoke。React UI 现在只通过 `window.__YUKINAL_HOST__` 与 C++ 宿主通信，不再回退 Tauri；`pnpm check` 移除所有 cargo 步骤并全绿，`pnpm package`（tauri build）与安装器脚本一并移除。C++ 宿主尚未实现的命令继续返回 `NOT_IMPLEMENTED`；重写指导见 `docs/rust-removal-plan.md`。

- C++ 桌面宿主接管本机 host-key 信任的只读与遗忘路径：`server_host_key_status`/`server_host_key_forget` 读写与连接路径同一个 `known_hosts` v1 文件，指纹变化报 `mismatch` 而不静默覆盖，遗忘未钉住的主机不伪报删除；探针与确认仍待迁移。桌面宿主测试覆盖 pin/status/forget 回环。

- Python Agent 按 ADR 0004 实现模型边界的工具名映射：内部点分名（`server.info`）对模型暴露为双下划线名（`server__info`），模型返回的调用名在查注册表前映射回内部名，并拒绝提供者名冲突；此前的点分名会被 OpenAI 兼容端点拒绝。跨语言 sidecar 集成测试改为在真实 `yukinal.db` 提供者存储中注册回环 Provider，并断言持久的只读路径；调查证据/产物持久化仍未在 Python 运行时实现，该测试不再把它计为通过。

- C++ 桌面宿主接管出站网络代理设置：`network_proxy_get` 读取直连/系统代理模式并只回报“凭据是否存在”，`network_proxy_save` 校验 `user:password`、在入参中擦除写入的凭据，并按轮换/留空保留/显式清除回收旧凭据；代理仍只改变连接路径，PAC 或内嵌凭据等不可用配置明确失败，凭据只进操作系统凭据库。C++ 宿主凭据边界改为可注入接口，桌面宿主数据库/IPC/self-check 测试在 Windows 本机通过。

- 桌面 Agent 的编辑与恢复在支持的密码/私钥 SSH 身份上改由 C++ SFTP 服务执行：发布前校验 size/mtime/SHA-256，在同目录独占暂存、复核并保留文件属性后使用 POSIX 原子重命名；缺少原子替换支持时失败关闭。其他认证路径仍使用兼容实现。
- 按“AI 目标执行、运维状态可视化、本机↔远端文件拖放/预览/传输”重订产品目标，增加跨模块施工顺序和 A/O/F/R 逐项验收标准；这是后续实施文档，不代表这些能力已经交付。
- 未发布施工分支已加入 `server.exec` 受控命令链及开发/预发布任务级预算委托、概览来源与新鲜度显示、流式文件传输内核与任务 UI、本机路径句柄/预览及原生拖入事件。新目标上传接入协商的 `hardlink@openssh.com:1` no-replace 发布，项目回环 SFTP 测试验证目标碰撞不覆盖、缺少扩展时失败关闭，以及发布后的 staging 清理残留记录；覆盖上传仍 fail-closed。真实 OpenSSH、远端拖出到系统文件管理器、崩溃后任务恢复、真实 Provider/MCP/服务器及三平台安装后 GUI 验收未完成。详见[目标架构与施工交接](./architecture-and-build-plan.md)中的工作树快照。
- 目标任务入口改为直接填写用户目标并显式选择服务器或本机；远程开发/预发布任务可主动勾选一次有限期/预算命令委托，默认逐项询问，切换到生产、未知、本机或非目标执行模式时不能携带该授权。新增桌面 Host RPC→SSH 回环验证和 SFTP `TransferManager` 发布回归；第 513 步时间线不会被前 512 条截断。验证覆盖本地授权/重放边界，不代表真实 Provider、MCP 或 OpenSSH 验收。
- 新增进程内 SSH 回环协议验收，验证主机密钥 pin、命令 stdout/stderr 与非零退出码、输出限额、超时和取消；隔离临时目录的真实 SFTP 子系统验收除底层读写外，还通过 `RemoteFileService` 覆盖备份碰撞防覆盖、多链接/未知链接数拒绝、revision 守卫编辑、过期恢复拒绝、有效恢复与校验后清理。替换前复核增加内容 SHA-256；故障注入将目标改成相同长度并恢复原 mtime，确认 rename 前仍以 `ConcurrentChange` 拒绝、并发内容保留且 staging 临时文件清除。SFTP 仍不是 CAS，最后一次内容复核后到 rename 的窗口仍存在。它们补足协议和文件服务组合证据，但不替代 OpenSSH 兼容性、真实目标机、桌面 IPC/AppState 或 UI 业务路径验收。
- 此前阶段曾按“只读诊断、受控变更、文件恢复”梳理产品与发布验收；本轮目标定义已以上述新三条路径取代其产品验收口径，原回归仍保留。
- 将固定版本且按 SHA-256 校验的 Node.js 运行时纳入桌面安装包；本机 Windows NSIS/MSI 构建成功，当前 MSI 管理员映像载荷启动/关闭冒烟通过。针对早先 NSIS 卸载曾删除数据目录的风险，明确维持产品规则：卸载永远保留数据库、设置和凭据；即使 Tauri 确认页的删数复选框被勾选，hook 也会阻止递归删除并在交互卸载时解释如何卸载后手动清理，静默与升级路径同样保留。
- 补强 NSIS 卸载回归保护：静态检查锁定 hook 存在、用户勾选后的说明提示、复选框状态清零、Roaming/LocalAppData 两条且仅两条删除语句仍受保护 hook 与非更新条件约束，并拒绝缺失状态捕获和额外无条件删除。Windows UI smoke 从 Tauri 生成脚本提取真实回调，操作原生 checkbox，验证默认未选、勾选提示、勾选与未勾选时 canary 均保留，更新模式也保留；另在 `/S` 静默模式通过测试专用参数预置“已勾选”状态，确认卸载正常退出且 canary 保留。所有破坏性验证只发生在临时 fixture。真实 native checkbox UI smoke 已在 GitHub Windows package run `36667041840` 通过；同一 run 也通过实际 `/UPDATE` 安装器重跑和数据保留断言。
- 扩展 NSIS 安装生命周期冒烟：在隔离目录真实安装后，以 `/UPDATE` 对同一安装执行就地更新，先移除再核实安装包恢复 `NOTICE` 文件且 SHA-256 与更新前一致，并验证 Roaming/LocalAppData canary 跨更新和卸载均未改变；GitHub Windows package run `36667041840` 已通过此更新生命周期、manifest 校验和产物上传。
- 修正两个 macOS Node.js `.tar.gz` 校验值与归档名错配的问题；打包前新增六平台官方 SHASUMS 全量校验，当前六个 pin 均已核对通过，Windows 打包前置流程与 Tauri release 编译（`--no-bundle`）通过。
- 为跨平台安装包产物生成 SHA-256 与构建来源清单；CI 上传前复核，并由门禁确保上传规则覆盖所有清单支持的安装包后缀。
- 新增受限的 MSI 安装/冒烟/卸载验收脚本和 NSIS 生命周期脚本，并在安装前拒绝触碰既有 Yukinal 数据目录。GitHub Windows package run `36667041840` 已通过 MSI 与 NSIS 的真实隔离安装、启动、关闭和静默卸载/数据保留步骤；runner 本身装有 Node，因此应用侧以受限 PATH 验证随包运行时，但仍缺整机完全未安装 Node 的复验。
- 扩展 Windows 包冒烟支持在 Agent 就绪后强制终止应用父进程；最新 MSI 提取载荷的正常关窗 1 次、强制终止 3 次均未遗留 sidecar。
- 为可选真实 Provider 验收补上 OpenAI 兼容 Chat Completions 与 Responses 两种 live 测试入口；真实服务调用仍待测试账号和端点。
- 补上 Host RPC 连接关闭时对全部挂起请求的拒绝与在途工具取消回归；交付文档现区分已自动覆盖的故障分支与仍需真实远端/完整 UI 环境验收的边界。
- 本机 `pnpm check` 全套门禁通过；PR #3 的三平台代码门禁、依赖审计和发布包 run `36674149301` 均成功。发布包验收覆盖 Windows MSI/NSIS 生命周期与数据保留、Ubuntu `.deb` 安装/purge、macOS DMG 挂载/复制，以及三平台随包 Agent 握手。Windows runner 安装了系统 Node；干净无 Node 主机验收、代码签名/公证、真实 SSH/Provider/MCP 运维流程及完整桌面 UI 工作流仍未完成。
- A/O/F/R 新产品路径在可重置真实环境及三平台安装版上的端到端验证仍待完成。
- 2026-09-30 本轮工作树 `pnpm check` 全绿，`pnpm package` 成功生成当前 dirty 工作树的 unsigned Windows MSI/NSIS 包，并通过 manifest 与 NSIS 生成脚本检查；本轮未实际安装新包。自动门禁和构建结果不替代真实 Provider/MCP/SSH、三平台原生文件操作和安装后业务验收。

## 1.0.0 — 2026-09-13

- 建立 Tauri/Rust 宿主、React 工作区、Node.js Agent sidecar 与共享 IPC 契约的版本基线。
- 提供 SSH 服务器管理、健康快照、终端、远程文件、服务与日志、Agent 对话和受控调查任务的实现。
- 建立权限审批、持久任务计划、活动记录、凭据隔离和本地/回环测试覆盖。

`1.0.0` 是代码接口基线；安装包仍未完成三平台真实安装验收。当前交付条件见[交付与发布](./release.md)。

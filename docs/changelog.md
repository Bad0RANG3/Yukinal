# 版本与发布历史

版本号遵循语义化版本，并且是**唯一**的：六个 `package.json`、`tauri.conf.json`、Cargo workspace 与 IPC fixture 里的版本由 `packages/shared/src/version.test.ts` 钉在一起（清单是它的 `MANIFESTS`）。从 `1.0.0` 起，Tauri IPC 命令、sidecar JSON-RPC 方法与跨层类型构成对外接口；破坏性变更只在下一个主版本发布，并记录在本节。

## 未发布

### 新增与改进

- **宿主命令层按职责拆分。** `commands/host.rs`（6,561 行）拆为根模块加 `host/` 子模块：`tools`（工具执行体）、`evidence`（证据/发现/决策摘要/留存）、`plan`（ChangePlan 与 guardrail）、`context`（`host.context.fetch` 投影）与 `tests`；根模块只保留协议分发、请求/响应类型与共享的失败包装。`commands/mcp/oauth.rs`（4,003 行）的非测试代码与 OAuth 测试分开（`mcp/oauth/tests.rs`），`commands/mcp.rs`、`commands/investigation.rs`、`commands/mod.rs`、`crates/filesystem/src/service/remote_file_service.rs` 的测试模块同样独立成文件；`crates/database/src/repositories/investigations.rs` 按任务/运行、证据、计划、调度拆成多个 `impl` 子模块；`crates/database/tests/persistence.rs` 拆为共享 fixture 加服务器/Provider、调查任务两个验收文件。拆分只移动代码与调整可见性，不改行为：完整 `pnpm check`（含跨语言测试）保持绿色。

- **安全止血（外部审计第一阶段）。** 桌面端 SSH 首次连接不再自动信任：终端、文件、SFTP、日志、服务与概览采集统一要求已知主机指纹（`RequireMatch`），未知主机在认证前失败（认证处理器收不到密码或 keyboard-interactive 回答），会建立连接的面板上直接提供「探针 → 对照 → 信任」的核验闸门，取代原先的 TOFU 行为（见 [ADR 0071](./adr.md#adr-0071桌面端-ssh-首连不再自动信任主机密钥)）。stdio MCP 与 sidecar 子进程改为清空继承环境、只保留显式白名单（第三方 MCP 拿不到 `SSH_AUTH_SOCK`、云厂商密钥与开发 Token），MCP 参数与环境变量增加数量/长度上限。Provider 地址非回环强制 HTTPS、禁止跟随重定向、保存时在命令层二次校验；终端广播 `Lagged` 不再停止转发，而是发出结构化的输出丢失通知；Provider SSE 增加单行/累计上限，sidecar 与 MCP 的 NDJSON 帧读写都加 8 MiB 上限。

- **Rust 资源生命周期审计与修复。** 明确 Rust 的安全保证不覆盖 `Arc` 循环、后台任务保活和外部句柄收口，并按这条边界修复 MCP HTTP GET 流、MCP/sidecar supervisor、终端 forwarder、SSH keepalive 与 Collector 取消路径：长等待任务只持 `Weak` 或配置快照，停止走取消令牌/显式关闭，`Drop` 作为同步兜底。新增本地回归覆盖最后 HTTP 句柄释放、重启退避不保活 supervisor、supervisor 释放回收 sidecar、管理器释放停止终端转发器，以及取消时终止本地进程组和远端 SSH 命令；真实公网 SSH、Provider、第三方 MCP 和长时间资源曲线仍未验证。生命周期契约见[架构总览](./architecture.md#架构总览)，未验收边界见[当前限制](./limitations.md#当前限制)，决策见 [ADR 0070](./adr.md#adr-0070后台任务弱引用加显式取消)。

- **副作用执行先过 durable plan 栅栏。** Agent registry 与 Rust host dispatcher 现在都拒绝没有完整 `taskId`、`planId`、`planStepId` 的 filesystem 写入、备份/恢复、重启、包安装和 MCP 调用；普通聊天不能通过伪造自动票据直接写入。交互式终端继续作为用户明确输入的人工旁路，不进入 Agent 自动任务链。会话授权同时绑定规范化输入的 SHA-256 指纹，改路径、容器、服务或包版本会重新请求授权。

- **持续巡检控制面补齐。** 排查任务页可以调整已有只读巡检的常用间隔和通知策略，暂停、恢复与不可逆撤销都复用宿主状态机；撤销触发器不能被 UI 重新激活，只能新建一个触发器。自定义但仍在宿主上限内的间隔会保留为可选项，运行历史和失败原因继续显示在任务时间线；带证据的成功历史运行可以由用户设为固定比较基线，宿主校验 schedule 归属和证据存在，基线失效时安全报告证据不足，不会静默换基线。调度器启动 sidecar 前还会复核任务终态，终态任务的到期触发会记录 `skipped/task_terminal` 并自动撤销 schedule。
- **固定基线可安全恢复默认。** 持续巡检设置现在接受显式 `baselineRunId: null`，用户可以清除固定基线并回到“最近成功样本”的比较策略；省略字段仍表示不修改，宿主区分两种语义，避免旧客户端更新时误清除基线。
- **安全与一致性审计重构。** 远程 Markdown 图片改为点击后才加载，而且同意的 URL 变化即失效；HostKey 探针签发绑定端点、指纹和短 TTL 的一次性票据；`known_hosts` 改为同目录临时文件加原子替换，写入失败回滚内存，提交后的目录同步失败不再制造内存与磁盘分叉；MCP 退出改为并行执行且带全局截止时间。Provider 换 key 使用新引用并在失败时清理，旧 key 只会在没有其他 Provider 引用时回收；服务器新增失败会回收身份；凭据先解除数据库引用，再删除或在 SQLite 持久清理队列中重试。GitHub Actions 固定到提交 SHA，并加入 RustSec advisory job；`Secret` 析构时清零。根清单中未使用的 `@deepseek-ai/dsh` 与 `web` 依赖被移除，锁文件减少约 490 个包。

- **Provider 高级配置接通。** Anthropic `apiVersion` 与受控的非敏感自定义请求头可从设置页保存、写入数据库并下发到运行时；`apiVersion` 只允许 Anthropic，自定义头在 UI、Zod 与 Rust 三层共用同一套边界，OpenAI-compatible 的网络异常也已经过统一脱敏。
- **MCP 生命周期与工具审核补齐。** 取消会发送标准 `notifications/cancelled`；崩溃的服务器会按有界退避重建进程与工具目录，状态报告尝试次数与耗尽，恢复不重放中断的调用。设置页可在服务器启动后审核工具，只有 `allowedTools` 中的工具会进入 Agent 目录，调用仍保持 `critical` 逐项审批。官方 `@modelcontextprotocol/server-everything` 已通过显式启用的真实互操作测试，长工具名会稳定缩短并在调用时还原远端原名。
- **MCP Streamable HTTP、静态认证头与 OAuth。** 设置页可在 stdio 与 HTTP 之间选择；HTTP 支持 `Mcp-Session-Id`、协议版本头、JSON 与 SSE 的 POST 回包、可选 GET 事件流、取消通知和 DELETE 会话终止。可配置最多 16 条有序的非保留静态认证头，编辑时可逐项保留、重排或轮换。OAuth 支持手填 issuer，也会从 `WWW-Authenticate` / RFC 9728 protected-resource metadata 自动发现 issuer，再走 RFC 8414 discovery + authorization code + PKCE S256；client id 留空时通过 RFC 7591 注册 `token_endpoint_auth_method: none` 的公共客户端。浏览器回调只监听随机 `127.0.0.1` 端口，token bundle 只进系统凭据库，过期前自动刷新，401 会强制刷新并仅重试一次。替换或删除服务器会回收旧凭据。远程 endpoint 强制 HTTPS，明文 HTTP 仅限回环地址，且拒绝 URL 内嵌凭据、查询参数、fragment 与重定向。按请求动态签名仍是明确限制。
- **MCP OAuth 增加 RFC 8628 设备码流程。** 每个 HTTP endpoint 可选 authorization code + PKCE 或 device code。设备码不需要本地回调：连接时界面显示服务器给的 `user_code`、优先打开 `verification_uri_complete`，客户端按元数据里的 `interval` 轮询，`authorization_pending` 与 `slow_down`（间隔 +5 秒）都只是继续等，`access_denied`、`expired_token`、`expires_in` 到期与用户取消各自结束并各自说明原因。轮询跑在那个等待中的 `mcp_oauth_connect` 请求里，没有后台任务；新增 `mcp_oauth_cancel` 让它随时可停。设备码流程注册的公共客户端带 device grant 且不登记 `redirect_uris`，两条流程共用同一个 token bundle 与刷新路径；流程与 issuer、client id、scopes 一样属于身份，改动会让已存令牌失效并要求重新授权。服务器没声明 `device_authorization_endpoint` 时直接拒绝，不回退到浏览器回调。
- **证据新鲜度投影。** 任务页、sidecar 调查上下文、证据搜索和单条取回现在都会显示宿主按 `default-v1` 评估的 `fresh`/`stale`/`expired`/`unknown` 状态、主机评估时刻、年龄和边界。旧证据不会被删除，模型提交的同名字段不会被信任；stale/expired 只作为历史上下文，变更前必须重新采集。
- **停止确认的 durable fence。** `agent.run.stop` 收到 sidecar 的 `stopped: true` 后，宿主立即复用终态同步路径封存对应 durable run/任务；迟到的 `agent.completed` 不会把停止后的任务重新打开，调度运行也会沿同一条取消路径收尾。这个 fence 只记录取消，不会复用旧审批或计划，也不声称能撤销已经在途的远端副作用。
- **远程日志与服务发现进入 Agent 排查链。** `server.logs` 和 `server.services` 复用桌面端已有的有界 journal/system log、systemd/Docker 发现命令，通过 `host.tool.execute` 进入目标校验、计划只读 fence、自动证据记录和审计；`readonly_health` 现在按健康快照 → 日志 → 服务 → Docker 清单 → Finding/决策摘要 → 最终复核的顺序生成计划。目标主机、SSH 时序和外部部署仍未因此变成已验证能力。
- **真实 sidecar durable 读链路验收。** `crates/core/tests/sidecar_agent.rs` 现在用真实 Rust supervisor 启动打包 Node sidecar，并由 `127.0.0.1` SSE Provider fixture 驱动“生成计划 → `server.info` → 宿主保存证据 → 完成”流程；测试覆盖 host request/response、计划步骤收口和终态事件，同时明确不覆盖 Tauri 窗口时序或外部服务器。
- **持续巡检异常后的有限只读复核。** 调度器把上一轮宿主比较得到的 `changed`/`baseline`/`no_change`/`insufficient_evidence` 转成下一轮有界调查重点；变化时 Agent 先检索相邻证据、用宿主比较无正文差异，必要时对齐同轮来源并整理候选信号，再在原范围内采集新样本。该提示不扩大目标、工具、预算或权限，变化仍必须经过新证据和宿主审计，不能直接触发写入、重启或删除。
- **调度启动恢复与上一轮结果保留。** 应用重启时，SQLite 事务会同时封存进行中的 schedule run、对应 durable run 和未完成步骤，清除旧 `activeRunId` 并留下可重试的 transport 失败；claim 不再把上一轮终态覆盖成临时 `claimed`，所以 changed/no_change/baseline 的复核重点会传给真正启动的下一轮。该路径只在本地 SQLite 与受控 fixture 中验收。
- **同一运行的证据重复记录复用。** 宿主按任务、run、目标范围、来源、输入摘要、种类、内容类型、截断状态和内容哈希查找已保存观察，重复记录返回 canonical `evidenceId` 并向 Agent 标明复用；跨 run 保持独立样本，且这项保护只去重本地持久化，不替代远端采集抑制。
- **重复证据不再推进计划。** 如果同一 durable run 的重复记录发生在当前计划的证据步骤，Agent 会把宿主返回的 `reused: true` 转成不可重试的步骤失败，不写成功基线工件，并把“改用检索/比较/关联、调整采样条件后重规划或等待用户”的原因交回模型；已经发起的远端只读调用仍不会被事后假装取消。
- **计划绑定的只读调用增加执行前重复 fence。** 对 server、Docker、systemd、package 和 filesystem 的只读宿主工具，若请求绑定同一任务/计划/步骤，宿主会在远端执行前用逻辑动作指纹拒绝第二次相同采样；运行中的 observation window 验证步骤继续由采样间隔和截止时间允许后续样本，普通聊天读取和换新计划的重采样也不受影响。这样把“证据重复后的计划失败”与“下一次远端调用是否还应发出”分成两道可审计边界。
- **跨层停止/恢复状态 fixture。** 桌面 Rust 把任务状态同步与 run 持久化抽成可直接接收 `AppState` 的状态核心，并用同一份临时 SQLite fixture 串起取消收口、活动 run 接管和迟到终态拒绝；这补上了离线状态层的跨层验收，但不把真实 Tauri IPC、sidecar 进程时序或远端部署写成已验证能力。
- **任务页显示持续巡检运行历史。** 选中任务后可以看到最近调度运行的状态、比较结果和失败原因，通知消失后仍能从任务时间线回查异常来源。
- **证据比较器。** Agent 新增 `investigation.evidence.compare` 只读工具；宿主只允许比较同一任务、同一目标范围内的两条已保存证据，并返回有界 JSON 变化路径、文本行数、来源差异和新鲜度警告，不把原始值或新的现场采样带入上下文，也不推进计划步骤。
- **证据关联器。** Agent 新增 `investigation.evidence.correlate` 只读工具；宿主按同一运行 ID，或旧证据的最多 3600 秒时间窗，对齐同一任务/目标范围内的跨来源证据摘要，返回匹配方式、来源集合和新鲜度/截断警告，不返回原始正文，也不把确定性关联包装成根因结论。
- **证据候选信号整理。** Agent 新增 `investigation.evidence.triage` 只读工具；它只对最多 16 条已由宿主取回并脱敏的证据做有界规则计数，输出资源压力、服务/容器状态、日志模式和观测完整性候选，并在不同证据共同命中有限信号族时给出交叉候选，不返回原文、不生成证据、不推进计划，并明确标记结果不是因果根因。证据 search/compare/correlate/triage 在活动计划内也不会消耗当前远端步骤；回环 E2E 覆盖了先建计划、再做本地对齐和 triage 的路径。真实外部目标仍未验证。
- **决策摘要显式续接与任务级停止。** 选项可声明 `continue_readonly` 或 `start_plan`，用户选择后任务页自动复用统一启动入口；`stop` 现在由宿主封存任务并清除活动运行栅栏，任务详情页的 `investigation_task_stop` 也复用同一条取消链路，旧选项和 `wait_user` 继续等待用户，宿主响应返回归一化后的继续意图，不能由自然语言隐式授权。重复停止已停止任务幂等，终态任务不会被伪装成可停止；停止选项不会把关联计划误标为已批准。
- **决策摘要的计划批准门槛收窄。** 宿主现在只有在用户选择显式 `start_plan` 续接时才批准摘要关联的变更计划；`continue_readonly`、`wait_user` 和 `stop` 不会把旧计划或调度摘要隐式升级为行动授权。这样持续巡检的只读复核仍可自动续接，但必须重新选择计划才能进入行动路径。
- **失败恢复自动续接。** 宿主恢复事务清空旧 `activeRunId` 后，桌面端对重试、重新规划、继续恢复或无选项的恢复请求自动复用 `investigation_task_start`；停止、等待用户和先规划回退仍保持显式等待，启动失败只显示错误，不伪造新运行。
- **连续恢复链验收。** 临时 SQLite 持久化测试现在覆盖应用重开后的传输失败、第二次认证失败、第三次运行接管，以及两次旧 run 迟到终态都被 `active_run_id` 栅栏丢弃；只有当前运行可以把任务收口为完成。这仍是离线验收，不代表真实远端部署。
- **终态任务历史保留闭环。** Agent 新增只读的 `investigation.retention.preview`，任务页也可先预览旧的未引用证据与已替代阶段工件，再由用户明确确认清理；宿主只接受终态任务，在 SQLite 事务中重新检查引用、状态和截止时间，并写入审计活动。被 Finding、步骤、工件、决策摘要或计划引用的证据会被保护；Agent 没有删除方法，远端文件备份仍不自动删除或轮换，校园网验收只覆盖本地 SQLite、IPC fixture 和回环路径。
- **远端备份账本盘点与受控清理计划。** Agent 新增只读的 `filesystem.backup.list`，只投影当前调查任务的宿主账本元数据，不把 `available` 误报成远端路径仍存在；`investigation.playbook` 新增 `backup_cleanup` 单项计划，绑定 path、backupPath 和 revision，经过用户逐项批准、宿主远端一致性检查后清理，并再次读取账本验证。后台自动轮换仍未启用。
- **受限 playbook 的部署后观察窗口。** `investigation.playbook` 可在配置编辑、重启、包安装、组合部署或只读健康计划的最终验证之后声明有界 `observationWindow`；sidecar 只接受最终验证步骤已经允许的只读工具，宿主持有窗口状态、采样间隔、截止时间和异常收口，任务页同时展示本地化状态、限定工具、成功标准和最近异常，不会借窗口追加写操作或自动回退。真实外部部署稳定性仍未验证。
- **观察窗口的有界自动续采样。** 当宿主返回下一次采样时间时，sidecar 不再因为模型提前结束文字回合就把窗口留在半途；它只会在同一运行内等待并重复原验证工具和原参数，重新经过计划、权限、证据和审计路径，直到宿主标记窗口成功或异常。没有有效时间表、运行预算耗尽或 sidecar 重启都会失败关闭/等待恢复，不会扩大工具或动作范围。
- **任务级宿主硬边界。** 任务创建页可声明 UTC 时间窗、禁止的内部工具和禁止的绝对路径前缀；这些 guardrails 会随任务持久化并出现在 Agent 上下文中，宿主在启动、计划检查和真实工具执行前再次复核。时间窗外、禁止工具或路径会以结构化策略拒绝结束，不会被重规划、换 callId 或 `..` 路径绕过；旧任务迁移为无额外限制。
- **任务级受限自动委托入口。** 排查任务创建页现在可选择 `ask` 或 `auto`；只有远程开发/预发布的 `goal + execute` 任务会保留 `auto`，其余目标在提交前归一化为逐项询问。明确的 `auto` 任务现在会让 medium 风险的 `filesystem.backup` / `filesystem.edit` 配置 action 在宿主复核后由 Agent 推进；`backup_cleanup`、`filesystem.restore`、容器/systemd 重启和包安装仍逐项批准，宿主也会拒绝 sidecar 伪造的免审批步骤。
- **受限 auto 任务自动启动。** 用户明确选择合规的 `auto` 目标任务后，创建成功会直接复用 durable `investigation_task_start`；只读任务仍安全自动启动，`ask`/`plan`/不合规环境仍停在待开始，避免把自动启动误变成隐式授权。
- **受限 auto 任务的有界失败恢复。** 任务页只对合规 auto 目标的可重试 transport/timeout 失败调用既有宿主恢复事务，并受任务 `maxAttempts` 限制；恢复会使旧审批、基线和观察窗口失效，非重试失败、停止、回退和高风险路径仍交给用户。
- **MCP OAuth 支持客户端密钥（`client_secret_post` / `client_secret_basic`）。** 设置页多了一个客户端认证选择与一个只写的 secret 输入框：留空保留已存值、重填即轮换、换方式或删服务器会回收，界面从不回填（服务端也不会把它送回来）。密钥只进系统凭据库，SQLite 与设置响应里只有引用，`Debug`、错误与日志里没有它；缺失时连接在发出任何 token 请求之前停下并说明去哪补。code exchange、device authorization、每次设备码轮询与 refresh 共用同一条认证路径，所以续期不会悄悄变成匿名请求；`client_secret_basic` 只把凭据放在 `Authorization` 头里，`client_secret_post` 只放在请求体里（RFC 6749 §2.3 的一次一种方式，fixture 会拒绝两处都发的请求）。动态注册的公共客户端不变：注册响应里若带回 secret，整次注册会被拒绝而不是降级。切换认证方式会让已存令牌失效并要求重新授权。
- **`filesystem.edit` 普通文件改为原子替换。** 内容检查仍使用 revision，但替换阶段先写同目录临时文件，再通过 SFTP rename 发布，避免读者看到半写文件或进程中断留下截断文件；symlink 与不支持的服务器保守回退原位写入。整条守卫仍不是 compare-and-swap，检查与替换之间并发修改仍可能被覆盖。（这条回退在后续的 ADR 0017 里被移除，见本节的「`filesystem.edit` 的替换阶段有守卫」。）
- **`filesystem.edit` 的替换阶段有守卫，且不再有静默回退**（[ADR 0017](./adr.md#adr-0017filesystemedit-的替换阶段有守卫但仍不是-compare-and-swap)）。替换与读取之间的窗口被缩到「读取后的 `stat` → rename」，窗口里的改动变成一条可区分的并发修改错误（发布之后再 stat 一次，确认发布出来的就是刚写进去的那一份）；`symlink`、`nlink > 1`、远端不报告链接数（Windows 上的 OpenSSH 就是如此）、服务器拒绝 rename、staging 建不出来，都会返回 `unsupported` 并**什么都不写**，而不是换成原位写。`mode`、`mtime`、`owner`、`group` 会在 rename 之前带上并逐项复核，保不住就点名失败。硬链接探测走一次远端 `stat` 探针（GNU `-c %h`，退回 BSD/macOS `-f %l`），路径按 POSIX 单引号规则引用。同一秒内、同样大小的改写仍不可区分，这一点写在限制里而不是藏起来。
- **`filesystem.backup` / `filesystem.restore` 形成受限恢复闭环**（[ADR 0043](./adr.md#adr-0043文件备份与守卫恢复只由宿主决定路径)）。编辑 playbook 会先逐项审批宿主生成的同目录独占备份；备份限制为 512 KiB 的普通单链接文件，并把服务器、目标、revision、任务归属和生命周期写入 SQLite；恢复必须命中同一任务的可用账本记录、当前 revision 和独立的 high-risk 用户批准，成功后记录被消费。没有任意 destination、自动回退或自动清理，真实外部目标仍未验证。
- **证据检索与调度预算边界。** 宿主给 sidecar 的调查上下文只投影证据元数据和阶段工件摘要；Agent 可再用 `investigation.evidence.search` 按来源、种类、时间窗和任务目标检索不含正文的证据摘要，再按真实 ID 显式取回单条原文。上下文、搜索和取回均是宿主校验的本地只读路径。持久化调度器现在按任务预算与调度预算逐字段取小值，调度规则只能收窄一次运行的安全包络，不能扩大任务预算。
- **Agent 支持 UTF-8 文本文件与 PDF 附件。** 输入框可选择、粘贴或拖入有界文本文件；内容按 UTF-8 与控制字符规则校验，随消息持久化，并在 Provider 边界展开为明确分隔的文本块。PDF 按 `%PDF-` 魔数、单文件 3 MiB、最多 2 个校验，并与图片共享 5 MiB 原始字节预算；Chat Completions、Responses、Anthropic 与 Gemini 各自转换成原生文档块。文本单文件 256 KiB、总计 512 KiB、最多 4 个；音频与任意二进制文件仍不在支持范围内。
- **Host certificate KRL 支持在线下载与独立签名者轮换。** 服务器配置可在本地 KRL 路径与 HTTPS URL 之间二选一，并额外信任最多 8 把独立 KRL 签名公钥；在线下载拒绝明文远端、URL 凭据/query/fragment、重定向和系统代理，流式限制为 16 MiB，下载或解析失败时连接失败关闭。`KRL_SECTION_SIGNATURE` 必须位于末尾，每条签名都要有效，且至少一条要由 host CA 或配置的独立 signer 签署。轮换期间同时保留旧、新 signer 即可，不再依赖远程自动更新。
- **Markdown 块引用与列表语义补齐。** 引用末尾仍为段落时，未带 `>` 的续行会留在引用中；下一行若是标题、列表、代码块、分隔线或另一引用起点则立即停止，避免吞掉引用后的正文。列表会按项间空行与项内直接块空行区分紧凑/宽松，紧凑项不再生成多余的段落包装，且换列表标记会正确开始新列表。CommonMark 的其余边角仍不承诺完全一致。
- **SSH 证书认证可在界面配置。** OpenSSH 用户证书路径和可选私钥路径作为非敏感元数据保存，私钥与口令仍只进系统凭据库；服务器出示 host 证书时，探针和信任流程现在钉住证书内公钥的指纹，而不是一律拒绝。
- **Agent 投递契约接通桌面端。** `duplicate` / `resumed` / `result` 会穿过 Rust IPC；面板先以 `resume: false` 受理消息，再用同一 `messageId`/`runId` 恢复，失败重试不会重复插入用户消息或分叉运行。运行设置里可选择流式投递或等待终态结果。
- **Agent 图片输入端到端可用。** 输入框支持选择、粘贴与拖放 PNG、JPEG、WebP、GIF，并在发送前按文件魔数校验；附件会随用户消息持久化。共享 Zod、Rust IPC 与数据库使用同一组上限，OpenAI Chat Completions / Responses、Anthropic 与 Gemini 各自转换成原生图片 block，纯图片消息也可运行。admission 指纹覆盖图片字节，同一 `messageId` 不能用不同图片恢复。
- **Node.js 版本预检。** sidecar 启动前对正常 Node 路径执行有 5 秒上限的 `node --version`；低于 24 或输出不可解析时会直接给出安装新版本与 `YUKINAL_NODE` 的提示，而不是等 Node 在解析 bundle 时失败。显式 `YUKINAL_AGENT_COMMAND` 不假定为 Node，因此保持不探测。
- **SSH keyboard-interactive 多因素认证。** 密码、私钥、用户证书或 ssh-agent 第一因素被服务器标记为 partial success 后，客户端继续完成有界的 keyboard-interactive 回合；桌面弹窗只持有当前提示与响应，响应经一次性 Rust 通道直接回送，不落 SQLite、keychain、活动记录或日志。取消会终止本次认证，120 秒超时、8 个回合与 16 个提示均有硬上限。
- **ssh-agent 签名失败可分类。** 直接匹配 russh 公开的 `AgentAuthError::{Send, Key}`，把 SSH 通道中断、agent 拒绝签名、agent 协议异常和 agent 交换失败分别映射；不再把“连接在签名途中断了”误报成“agent 拒绝了身份”。
- **Host certificate CA/principal 信任。** 每台服务器可保存 OpenSSH CA 公钥与最多 32 个 principal 模式（支持 `*`/`?`）；启用后客户端显式广告证书算法，并校验 host 证书类型、签名、CA 公钥、有效期、critical options 与 principal，普通 host key 无法降级绕过。未配置 CA 时仍沿用原有 TOFU/叶子指纹策略。
- **Host certificate KRL 撤销（初始实现）。** 每台服务器可配置本地 OpenSSH KRL；客户端解析证书序列号列表/范围/位图、key ID、通配或指定 CA、显式公钥与 SHA-1/SHA-256 指纹撤销，并在 CA 签名和有效期校验后拒绝命中的 host certificate。当时只接受未签名 KRL；在线下载与 CA 签名验证已在本节后段补齐。
- **Markdown 与外部内容。** 新增 setext 标题、缩进代码块、引用式链接与脚注；`http(s)` 链接经受限 opener 打开。远程图片默认只显示替代文字和显式加载按钮，用户点击后才以 lazy/no-referrer 加载。流式文本与最终文本不再二选一：前缀可扩展时合并，真正分歧时保留两段。
- **Linux 打包依赖基线。** `.deb` 与 `.rpm` 明确声明 WebKitGTK/GTK 依赖，打包契约拒绝空依赖表；跨发行版安装仍需真实验证。
- **IPC fixture 两侧闭合。** 之前只有 TypeScript 一侧解析的 provider、服务器 CRUD、终端、远程文件、Agent 停止/审批与 MCP fixture，现在都由 Rust 用真实响应类型反序列化、再序列化回原 JSON；字段缺失、改名或静默丢弃都会让门禁失败。
- **音频附件（WAV / MP3 / OGG / FLAC）。** 输入框可选择、拖放或粘贴音频，格式按**魔数**校验而不看扩展名与 MIME；单段最多 4 MiB、最多 2 段，并与图片、PDF 共用同一个 5 MiB 原始字节预算（受 8 MiB 单帧上限约束）。待发送区用 `<audio>` 播放器预览并可逐项移除，附件随消息持久化，历史恢复与重试沿用同一条路径；admission 指纹覆盖音频字节（新增 kind 现在会让指纹函数编译失败，而不是悄悄漏掉）。Provider 映射：OpenAI 的 `input_audio`（`chat` 与 `responses` 两种方言，只接受 WAV/MP3，其它格式在发请求之前就明确失败）、Gemini 的 `inlineData`（四种都收）、Anthropic 明确报「没有音频内容块」而不是静默丢掉。**这两条 OpenAI 形状尚未对真实 API 确认**，与两个原生适配器的整体状态一致。
- **Markdown 按 CommonMark 0.31.2 的实测差距收口。** 强调改成规范的两步算法（先记 `*`/`_` delimiter run 的左右贴边，再按「3 的倍数」与嵌套优先规则配对），容器里的续行（lazy continuation line）有了自己的身份，段落改为整段解析——换行因此也是行内语法，代码段与强调可以跨软换行，段落最后一行的反斜杠按规范留作普通文字；数字字符引用（`&#35;`、`&#x22;`，含 `U+0000` 与非法码位换 `U+FFFD`）开始解码。实测：官方语料在范围内的 463 个例子里 **434 个与规范文本一致、461 个没丢字**（此前是 367 / 434）。剩下 2 个丢字是**有意**的偏差：`&copy;`、`&quot;` 这类命名实体引用仍按字面显示——那要一张 2231 条的 HTML5 名字表，与「不为小功能引大依赖」冲突。顺带一处渲染变化：`***x***` 现在按规范第 14 条渲染成 `<em><strong>x</strong></em>`（此前是反过来的嵌套）。逐节数字、方法与偏差清单在 `docs/boundaries/markdown.md`。
- **MCP 的 OAuth 令牌可以绑定到本机密钥（DPoP，RFC 9449）**（[ADR 0018](./adr.md#adr-0018mcp-的-oauth-令牌可以绑定到本机密钥dpop但默认仍然只是-bearer)）。每台服务器可勾选「发送方约束令牌」：宿主生成一把 Ed25519 密钥并只存进系统凭据库（配置里只有引用），此后每个请求 —— POST、GET 事件流、DELETE，以及 token 端点的授权码兑换、设备码轮询与刷新 —— 都带一个 `dpop+jwt` proof（`jti` 每次重新随机、`htm`/`htu` 绑定方法与去掉 query 的 URL、带令牌的请求另有 `ath`）；服务器用 `DPoP-Nonce` 挑战时按 origin 记住并用新 proof 重试恰好一次，加上刷新重试，一次请求最多发三次。开启后服务器必须回 `token_type: DPoP`，回 `Bearer` 或不说类型都会在存下令牌之前失败，而不是悄悄退回 bearer；私钥读不出来（换机器、被清理）时报「需要重新授权」并回收令牌。开关与 issuer 一样属于身份，改动会让已存令牌失效、并回收旧密钥；删除服务器同样回收。测试里的本地授权服务器会真的验签、比对方法与 URL、检查时间窗、拒绝重复 `jti` 并挑战 nonce。**默认关闭**：不验证 proof 的服务器只是多收一个头。

- **三条「不做」的决定写下来了**（[ADR 0019](./adr.md#adr-0019按请求动态签名不实现先把协议定下来) / [ADR 0020](./adr.md#adr-0020host-krl-的信任分发仍留在本地静态配置) / [ADR 0021](./adr.md#adr-0021任意二进制附件不内联进消息)）。按请求动态签名不实现：签名方案必须服务端也采用同一套，客户端自创一套等于没人验证的自定义密码学协议，ADR 里写下了做之前必须满足的四条。Host KRL 的信任分发仍留在本地静态配置：任何「从 URL 拉一份新 signer 列表」都要先回答「这份列表由谁签」，而 OpenSSH 自己也没有 trust bundle 协议 —— 与其自造一套弱信任，不如保留「新旧 signer 并存」的本地轮换，ADR 里同样列出了真要做时的前置条件。任意二进制附件不内联发送：内联 base64 塞满帧、三家 Provider 的 Files 接口语义不同、把可执行文件与凭据文件自动上传是最坏的默认值之一。三份限制都写在 `docs/limitations.md` 里，而不是留成模糊的「以后可能支持」。
## 1.0.0 — 2026-09-13

首个稳定版本，冻结桌面端、Rust 宿主与 Node.js sidecar 之间的跨层接口，并补齐正式项目的文档、贡献与安全入口。

### 新增
- **Agent 展示 Provider 的思考与 token 统计**：`reasoning_delta` 不再混入回答，而是作为桌面端折叠的「思考过程」；累计 `usage` 显示在运行头部。
- **策略与环境不匹配时给出提示**：显式选择的 `policyId` 与目标环境不一致时，策略设置和工具栏会显示警告；提示不改变权限判定。

- **原生 Anthropic Messages 与 Gemini `generateContent` Provider**（[ADR 0011](./adr.md#adr-0011增加原生-anthropic-与-gemini-provider身份分支仍只允许发生在装配点)）：两套协议各自一个适配器，系统提示、工具调用与终止原因的翻译留在适配器内，agent loop 不按 Provider 身份分支。两者都能解析 token 统计与推理增量——OpenAI-compatible 那条路径两者都不产出。
- **`filesystem.edit`**：带内容摘要校验的读-改-写。读取返回内容的 SHA-256，编辑要求它仍然一致、且 `oldString` 恰好出现一次，否则拒绝而不是覆盖。竞争窗口被收窄，但没有关闭（SFTP 不提供事务）。
- **ssh-agent 与带口令的私钥**：桌面端第三种认证方式与口令输入框；口令材料只进操作系统凭据库，SQLite 只存引用。
- **主机指纹的手动核验入口**（[ADR 0012](./adr.md#adr-0012主机指纹默认-tofu但必须能人工核验钉住与遗忘)）：探针、信任、遗忘、状态四项；校验失败同时给出**已钉住的**和**出示的**两个指纹，而不是一个占位字符串。
- **sidecar 崩溃后的有界自动恢复**（[ADR 0010](./adr.md#adr-0010崩溃后的自动恢复有界且只恢复能力不复活状态)）：最多 5 次、退避到 30 秒、60 秒后视为健康；状态里的重启记录报告第几次、是否已放弃。用户按下的停止永远不会被回答成一次重启。
- **MCP stdio 客户端与接入**（[ADR 0014](./adr.md#adr-0014mcp-接入宿主独占进程与目录agent-只注册与调用)）：握手与版本协商、工具列表与调用、每次调用的超时、有界诊断尾部、退出记录、每个服务端单实例；服务器的新增/删除/启动/停止进了设置页，工具目录经新增的宿主方法交给 sidecar，外部工具以 `mcp.<服务器>.<工具>` 注册进同一个 registry 并一律声明为 `critical`。看列表不会启动任何进程，`http` 在类型层面被拒绝。
- **安装包**（[ADR 0013](./adr.md#adr-0013安装包分发随包分发-agent-bundle但使用用户自己的-node)）：启用打包并列出目标平台，agent 以单文件资源 `<resource_dir>/agent/index.js` 随包分发（同目录还有一份声明 ESM 的一行 `package.json`），Node.js 由用户自备（≥ 24）。
- **`delivery` / `resume` / `policyId` 的真实语义**：同步投递会等到运行结束并返回结果，登记而不执行会用同一个运行 id 在后续请求里真正启动，未知的策略 id 报错而**不会**回落到环境默认策略。
- **Agent 回复按 Markdown 渲染**（[ADR 0015](./adr.md#adr-0015agent-回复用自建的-markdown-渲染且不产生可导航链接)）：标题、列表、表格、围栏代码块过去都是原样显示的字符。解析器是仓库自己的纯函数（无新增依赖），只把文本变成数据结构，渲染层只创建元素——正文里的 HTML 显示为文字，链接与图片不产生可导航元素；未闭合的围栏按「流式到这里为止」处理。
- **Provider 可以删除了**：此前只能新增和启用，写错的一份配置就永远留在列表里。删除走行内二次确认，并说清两件从别处看不出来的事：密钥会不会被一并删掉（引用可以被多行共享，所以只有没有别的行还引用它时才回收），以及当前项被删后由谁接替。这条守卫在数据库层按**整张表**问，所以二次确认里那句话是字面成立的。
- **对话记录视图**：记录过去是一次取 50 条按更新时间平铺的卡片，归档后从列表里消失、删除时弹阻塞式确认框。现在按本地日历日分组（今天 / 昨天 / 最近 7 天 / 更早，组标题吸顶）、标题与消息正文的命中直接标在行里、默认只列进行中并显示三个标签各自的真实条数、每页 50 条往下翻、就地改名、删除改成行内二次确认（默认焦点在「取消」上，回车不会删掉东西）、方向键在行间移动、斜杠键跳到搜索框。行上的目标是服务器的**名字**，服务器已被删除时写「已移除的服务器」——不透明的 id 不上界面。

### 变更

- IPC 契约若干处与语义一起收紧：运行开始的响应新增是否真的开始与可选结果；Provider 保存从「只有一种协议」的命令名变成带 `kind` 的统一命令；文件读取的输出新增内容摘要；Agent 状态新增可选的重启记录。`0.1.0` 是预发布基线，不承诺跨版本兼容；从 `1.0.0` 起按语义化版本维护。
- 脱敏逻辑从 sidecar 私有提升为 crate 级能力，因为 MCP 客户端也要把陌生子进程的输出交给用户。顺带修掉三处真实漏检：`Authorization: Bearer <token>` 的顺序问题（token 曾原样留存）、`secret=` 未被当作敏感标记、以及显式方案名后短凭据的漏检。
- `apps/agent` 的项目关闭了 emit：它曾经把逐文件的 tsc 产物盖在 esbuild 的单文件 bundle 上，让「类型检查通过」之后打包契约反而变红。修在 `package.json` 的脚本上不够——手写一条 `tsc` 命令会绕过脚本，而这件事真的又发生过一次。
- `initialize` 里的 MCP 能力标志从字面量改成读注册表：它报告的是「此刻注册表里有没有 MCP 工具」，不是「这个构建支不支持 MCP」。握手时它通常仍为假，因为工具目录只能在 stdio 通道起来**之后**去取——那正是实话。
- **窗口里只有「你与 Agent 的对话正文」能拖动选中。** 原来整个界面都可选中，随手一拖就把侧栏、工具卡片和标签一起涂蓝，复制出来的内容里混着界面自己的字。代价是日志、文件预览、设置项里的文字不再能框选复制——这与「随手一拖不再涂蓝半个界面」是同一个取舍。
- 对话记录的列表响应从会话数组变成带分页与统计的对象，并新增重命名命令。统计与搜索词同一批数据，但**不**受归档筛选影响——否则标签上的数字会随点它自己而变化，那不是统计，是回音。
- 对话记录这一族的 7 份 IPC fixture 现在两侧都解析（Rust 侧把它们编译进来并断言序列化输出与之逐字节相等），此前这一族一份 Rust 断言都没有。文档里的 fixture 覆盖统计随之修正。
- **设置页里选 Provider 从一个列表收成一个选择框**：原来每个 Provider 占一行，十四个 Provider 就是十四行长条，而这一屏要回答的只有「我在看哪一个」「新建运行会用哪一个」。现在选择框决定前者（切它只换下面表单编辑的对象，**不**顺手切换启用的 Provider），一个按钮决定后者（已是当前时显示为禁用的「当前使用中」）。名称重复时选项补一个最短的区别：网关不同就补主机名，名称、模型、网关全都一样时用 id 尾部 6 位当记号。
- **文档合并成一份。** 原来分散在项目说明、文档目录、架构决策记录目录、打包说明、两个子模块说明与一份变更日志里的内容，现在全部在这份 `README.md` 里；那些文件已经删除。代码注释里指向它们的路径已经改指到本文的相应小节。
- **文档按「介绍」与「规则」重新分开。** 合并成一份的代价是：想读「这个项目是什么」的人要穿过一千行的契约与决策，而改代码的人要在一份文件里找自己那一条。现在根 `README.md` 只介绍项目（是什么、现在能做什么、怎么跑起来），**规则、约束与决策搬进 `docs/`**：架构、执行与授权模型、三档权限、安全边界、三个跨模块边界、限制、开发、打包、ADR、发布历史各自成篇，索引与文档治理规则在 `docs/README.md`。三档权限原本堆在授权模型一节里，现在 `docs/risk-tiers/` 下一档一份（`read` / `write` / `dangerous`）。内容本身是照搬的：搬动只重算链接，不改写断言；代码注释里指向 README 那些小节的指针随之改指到 `docs/` 的对应文件。

### 修复

- **认证材料从未送达过 SSH 后端**：凭据输入的枚举只重命名了变体、没有改字段名，于是 Rust 期待下划线拼写，而共享契约一直发的是驼峰拼写。私钥与身份两条保存路径在 IPC 上本来就不可能成功。
- **每一个宿主请求都会被执行两次**：事件转发器由每次启动 sidecar 创建，而 supervisor 不转发退出事件，于是崩溃重启后挂上第二个转发器。它现在是窗口级单例。
- **Provider 的未分类流错误会把原始错误文本交出去**：SSE 帧解析失败、读取中途的连接错误与传输失败都走同一条分支，它没有经过脱敏，而网关把收到的请求回显在错误里是常见事——密钥会因此进入事件流、界面与审计记录。现在与模型目录那条路径一样先脱敏再截断。
- **离开「服务器」栏目时 Agent 面板会向右跳 12 像素。** 有一条只作用于非服务器栏目的平移规则，而它没有补偿任何东西：两种轨道布局把面板左边缘放在同一个位置，收起时它又是绝对定位。删掉那条规则之后，同一张卡片在任何栏目里都停在同一个位置。
- **`filesystem.edit` 的正文过去会进审计记录**：审计输入掩掉了一个字段，却没掩旧文与新文，而它们同样是任意文件内容。两者现在同等处理。
- 一个工具**结果**事件若声称自己还在运行，原本会被当作终态落库，在审计里留下「已结束但还在跑」的记录。
- **MCP 子进程曾经只靠析构回收**：宿主正常退出时没有显式关闭它们，而析构在 Windows 上强杀宿主时不会执行，于是关掉窗口可能留下一串第三方进程。退出路径现在和 sidecar 一样显式关闭它们。
- **桌面应用在 Windows 上根本起不来 sidecar**：资源目录返回的是带 verbatim 前缀的规范化路径，而 Node 解析不了它——拿到那种路径时它会去访问盘符本身并以 `EISDIR` 退出，agent 一行都没跑，界面上只剩「sidecar 已退出」。交给 Node 的入口路径现在会先去掉有普通等价形式的两种前缀；同一处补上了这次的教训：启动失败时把 sidecar 死前写下的 stderr 打进日志，并在启动前打印解析出来的命令。
- **设置页显示的 sidecar 入口仍然是那条带前缀的路径。** 上面那条修的是命令行，而报给界面的标签还是从清洗前的路径生成的，于是一行读起来像「Node 读不懂、也从来没有真正被执行过」的路径，而这一行的用处恰恰是让用户确认「到底跑了哪个文件」。标签现在与命令行取同一条路径，并有一个用例钉住它。
- **`tauri build` 在本仓库从未跑通过，原因是构建钩子的运行目录。** 见 [打包与分发](./packaging.md#打包与分发)：钩子里的脚本名假定当前目录是仓库根，而 Tauri 在应用目录下执行它。现在用能解析到 workspace 根的写法，Windows 上的完整打包路径已经跑通并产出 NSIS 安装程序与 `.msi`。

## 0.1.0 — 2026-09-12

首个带版本号、可对照与可回退的开发快照。

### 包含

- 桌面工作区：服务器管理、连接、概览健康快照、终端、远程文件、服务与日志、活动记录、Agent 对话历史。
- Agent 运行时（Node.js sidecar）：完整的 agent loop、内置工具、三层风险事实与唯一授权入口 Permission Engine、审批往返、取消与墙钟上限。
- OpenAI-compatible Provider（Chat Completions 与 Responses 两种方言）。
- 浏览器预览模式：可以在没有 Rust 的情况下调界面，原生能力明确不可用。
- 完整校验门禁 `pnpm check`：公开文档卫生、凭据扫描、跨语言契约、类型检查、单元测试、sidecar 冒烟与 Rust 侧的格式、静态检查与测试。

### 尚未完成

以[当前限制](./limitations.md#当前限制)一节为准，它是**所有**已知缺口的唯一来源。

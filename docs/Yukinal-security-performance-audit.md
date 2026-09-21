# Yukinal 代码工程安全与性能审计建议

> 项目：[Bad0RANG3/Yukinal](https://github.com/Bad0RANG3/Yukinal)
> 审计范围：仅分析代码工程、安全边界、资源管理与性能，不评价 Star、贡献者或社区活跃度
> 审阅基线：`main` 分支提交 `5c551d74caff70887dfa22b2f27a37ce56f9e414`
> 审计方式：代码静态审阅、前端生产依赖审计、仓库秘密扫描；不等同于完整渗透测试

## 1. 总体结论

Yukinal 的安全工程意识明显高于一般个人项目：系统凭据库、SSH 主机密钥固定、MCP HTTP 约束、Tauri 权限隔离、日志脱敏、文件写入保护和输入大小限制等基础设施已经存在。

但它目前仍不适合被描述为“可放心管理真实生产服务器的成熟运维工具”。最需要优先收口的风险不是普通前端 XSS，而是以下系统级信任边界：

1. SSH 终端首次连接自动信任主机密钥；
2. stdio MCP 进程拥有当前用户的完整本机权限；
3. Provider 自定义地址可能导致 API Key 和运维数据外发；
4. 子进程树、网络流和 IPC 缺少完整的资源边界；
5. 高吞吐终端输出可能令事件转发永久停止；
6. 发布产物尚未建立代码签名和供应链证明。

如果先解决上述问题，项目的可信度和简历含金量都会提升一个明显档次。

## 2. 风险优先级总览

| 优先级 | 问题 | 主要影响 | 建议完成时间 |
| --- | --- | --- | --- |
| P0 | SSH 终端首次连接自动 TOFU | 首次连接可能遭中间人攻击 | 立即 |
| P0 | stdio MCP 以用户完整权限运行并继承环境 | 本地文件、SSH Agent、开发凭据可能泄露 | 立即 |
| P0 | Provider 地址允许远程明文 HTTP，Rust 入口未做同级校验 | API Key、Prompt、服务器信息可能外发 | 立即 |
| P1 | MCP/Sidecar 无完整进程树回收 | 孙进程可在停止或退出后继续运行 | 发布前 |
| P1 | SSE、Sidecar 帧及响应体缺少完整上限 | 内存耗尽和拒绝服务 | 发布前 |
| P1 | 本地数据目录和 SQLite 权限未显式收紧 | 对话、服务器元数据和审计数据泄露 | 发布前 |
| P1 | Markdown 远程图片可请求私网地址 | 内网请求、DNS 泄露、GET 副作用 | 发布前 |
| 发布阻断 | 安装包未签名、未公证 | 无法验证安装包来源和完整性 | 对外发布前 |
| 性能 P0 | 终端事件发生 `Lagged` 后转发任务退出 | 后续终端输出不再到达 UI | 立即 |
| 性能 P1 | 附件使用 Base64 跨多层复制 | 内存峰值和 IPC 开销较大 | 近期 |
| 性能 P2 | 单 SQLite 连接、全局互斥锁和模糊搜索 | 数据增长后出现阻塞和搜索退化 | 中期 |

## 3. 必须立即解决的安全问题

### 3.1 SSH 首次连接自动信任主机密钥

终端连接当前使用：

```rust
known_hosts_policy: KnownHostsPolicy::TrustOnFirstUse
```

相关代码：[`terminal.rs`](https://github.com/Bad0RANG3/Yukinal/blob/5c551d74caff70887dfa22b2f27a37ce56f9e414/apps/desktop/src-tauri/src/commands/terminal.rs#L92-L112)。

代码注释明确表示终端首次连接会自动信任并记录主机密钥。这与项目已经具备的 `probe → 用户确认指纹 → trust` 流程不一致。

#### 风险

- 首次连接被中间人劫持时，客户端会把攻击者的密钥记录为可信；
- 密码和 keyboard-interactive 回答可能被交给伪造服务器；
- 后续严格匹配只会固定第一次受到污染的结果；
- 用户容易误以为所有 SSH 入口都执行了相同的指纹确认策略。

#### 修复方案

- 所有 SSH 入口统一使用 `RequireMatch`；
- 未知主机必须在认证前停止；
- 界面展示主机名、端口、密钥算法和 SHA-256 指纹；
- 只有用户明确确认后才能写入 `known_hosts`；
- 指纹变化必须硬失败，不提供“仍然继续连接”；
- 终端、文件管理、Agent 工具和服务器探测共用同一套策略。

#### 验收标准

- 未知主机打开终端时，认证处理器不会收到密码或交互式认证回答；
- 用户确认后才能完成连接；
- 主机指纹变化时，所有 SSH 功能一致失败；
- 自动化测试覆盖未知、已知匹配、已知不匹配三种状态。

### 3.2 stdio MCP 实际上是运行任意本地代码

MCP 使用 `Command::new(program)` 启动第三方程序，并继承父进程环境。相关代码：

- [`handle.rs`](https://github.com/Bad0RANG3/Yukinal/blob/5c551d74caff70887dfa22b2f27a37ce56f9e414/crates/core/src/mcp/handle.rs#L235-L260)
- [`config.rs`](https://github.com/Bad0RANG3/Yukinal/blob/5c551d74caff70887dfa22b2f27a37ce56f9e414/crates/core/src/mcp/config.rs#L68-L78)

MCP 工具审批只能限制 Yukinal 是否发送 `tools/call`，不能限制 MCP 进程自行执行系统调用。

#### MCP 进程当前可能直接执行的操作

- 读取用户文档、源码、SSH 配置及浏览器数据；
- 访问 `SSH_AUTH_SOCK`；
- 读取环境中的 GitHub、AWS、数据库等 Token；
- 主动联网并上传数据；
- 派生不受 Yukinal 工具审批控制的其他进程。

#### 第一阶段修复

- 启动前展示程序绝对路径、参数、来源和权限说明；
- 调用 `env_clear()`，只传入必要环境变量；
- 默认移除 `SSH_AUTH_SOCK`、云厂商凭据和开发 Token；
- 为 MCP 设置独立、受控的工作目录；
- 限制参数数量、单参数长度和参数总长度；
- 将“允许运行 MCP 程序”和“允许调用某个 MCP 工具”设计为两层授权。

#### 第二阶段修复

- 默认拒绝未锁版本的 `npx -y package`；
- 要求明确版本，保存包来源及完整性哈希；
- 对运行时下载代码给出独立警告；
- Linux 使用 bubblewrap、Landlock 或 seccomp；
- Windows 使用 Job Object，并评估 AppContainer 或低权限 Token；
- macOS 使用适合的 sandbox profile；
- 文件、网络和环境变量访问权限分别授权。

#### 验收标准

- 测试 MCP 无法读取专门注入父进程的测试密钥；
- 默认环境中不存在 SSH Agent socket；
- 未授权 MCP 无法访问用户主目录；
- UI 明确区分“第三方本地代码”与“远程 MCP HTTP 服务”。

### 3.3 MCP 和 Sidecar 的子进程树回收不完整

当前 `kill_on_drop(true)` 和 `start_kill()` 主要作用于直接子进程，不能保证孙进程同时退出。项目的 collector runner 已有 Unix 进程组处理，但 MCP 和 Sidecar 没有统一复用。

#### 风险

- MCP 可以派生后台进程后自行退出；
- 用户点击停止后，后台进程仍可能读取文件或联网；
- 应用退出后可能遗留进程、端口和锁文件；
- Windows 上仅终止直接子进程尤其不可靠。

#### 修复方案

抽象统一的 `ProcessSupervisor`：

- Unix：启动独立 process group，停止时终止整个进程组；
- Windows：使用 Job Object 并启用 kill-on-job-close；
- 先发送正常终止信号，超时后强制结束；
- 应用正常退出、崩溃恢复、用户停止和启动失败走同一套回收逻辑；
- collector、MCP、Sidecar 共用实现。

#### 验收标准

使用测试 fixture 启动子进程并派生孙进程，确认以下操作后孙进程均不存在：

- 停止 MCP；
- Sidecar 重启；
- 应用正常退出；
- 握手超时；
- 启动流程中途失败。

### 3.4 Provider 地址可能导致密钥外发

共享 Schema 目前只检查 URL 是 `http/https` 且没有内嵌用户名和密码，远程明文 HTTP 仍能通过：[`provider.ts`](https://github.com/Bad0RANG3/Yukinal/blob/5c551d74caff70887dfa22b2f27a37ce56f9e414/packages/shared/src/schemas/provider.ts#L22-L29)。

Rust 的 `provider_save` 最终主要执行字符串清理并保存，没有重新验证协议和目标主机：[`provider.rs`](https://github.com/Bad0RANG3/Yukinal/blob/5c551d74caff70887dfa22b2f27a37ce56f9e414/apps/desktop/src-tauri/src/commands/provider.rs#L227-L239)。

Node Provider 会直接向该地址发送带 API Key 的请求，且没有显式关闭重定向：[`openai-compatible.ts`](https://github.com/Bad0RANG3/Yukinal/blob/5c551d74caff70887dfa22b2f27a37ce56f9e414/apps/agent/src/providers/openai-compatible.ts#L39-L49)。

#### 风险

- API Key 通过远程 HTTP 明文传输；
- 错误、恶意或被导入的 Provider 地址接收 API Key；
- Prompt、服务器信息和工具结果同时泄露；
- 重定向使最终接收方与界面展示地址不一致；
- 内网 Provider 配置可演变成 SSRF 通道。

#### 修复方案

- 非 loopback 地址强制 HTTPS；
- `127.0.0.1` 和 `::1` 可允许 HTTP，但必须防 DNS 重绑定；
- 默认设置 `redirect: "manual"`；
- 如确需重定向，只允许同源跳转；
- 保存配置时展示“API Key 将发送到哪个 origin”；
- TypeScript Schema、Rust command 和 Node Provider 共用同一套规则；
- `/models` 响应先限制字节数，再执行 JSON 解析；
- 对敏感环境可增加 Provider 主机白名单或首次连接确认。

#### 验收标准

- `http://example.com` 被拒绝；
- `http://127.0.0.1` 可按策略允许；
- 跨源 301/302 不携带 API Key；
- DNS 解析到私网、链路本地或元数据地址时被拒绝；
- 绕过前端直接调用 Tauri command 也无法保存非法地址。

### 3.5 Markdown 远程图片仍可请求内网地址

远程图片默认不加载且需要用户点击，这一点设计正确。但批准逻辑只判断批准 URL 与原 URL 是否相等，随后直接设置 `<img src>`：[`MarkdownText.tsx`](https://github.com/Bad0RANG3/Yukinal/blob/5c551d74caff70887dfa22b2f27a37ce56f9e414/apps/desktop/src/components/MarkdownText.tsx#L227-L262)。

#### 风险

模型输出可能诱导用户加载：

- `http://127.0.0.1:...`；
- `http://192.168.x.x/...`；
- `http://169.254.169.254/...`；
- 公司内网管理接口；
- 带唯一标识符的跟踪 URL。

即使响应内容无法读取，也可能触发 GET 副作用或造成 DNS 泄露。

#### 修复方案

- 禁止 loopback、私网、链路本地、云元数据地址和 `.local`；
- 禁止远程明文 HTTP；
- 不让 WebView 直接加载远程图片；
- 由 Rust 代理下载并重新验证 DNS 和重定向目标；
- 限制 Content-Type、单图大小、总量和下载时间；
- 下载成功后以本地 blob 或受控协议展示。

同时建议收紧 CSP：

```text
object-src 'none';
base-uri 'none';
frame-src 'none';
form-action 'none';
```

并移除当前宽泛的 `img-src http:`。现有配置见 [`tauri.conf.json`](https://github.com/Bad0RANG3/Yukinal/blob/5c551d74caff70887dfa22b2f27a37ce56f9e414/apps/desktop/src-tauri/tauri.conf.json#L25-L28)。

### 3.6 本地数据文件缺少显式权限策略

启动时主要通过 `create_dir_all` 创建目录，SQLite 使用普通 `Connection::open`：

- [`state/mod.rs`](https://github.com/Bad0RANG3/Yukinal/blob/5c551d74caff70887dfa22b2f27a37ce56f9e414/apps/desktop/src-tauri/src/state/mod.rs#L64-L73)
- [`database/lib.rs`](https://github.com/Bad0RANG3/Yukinal/blob/5c551d74caff70887dfa22b2f27a37ce56f9e414/crates/database/src/lib.rs#L85-L117)

虽然密钥保存在系统凭据库中，但数据库仍包含对话历史、服务器地址、用户名、工具执行记录、调查证据以及 Provider/MCP 元数据。

#### 修复方案

- Unix：数据目录强制 `0700`；数据库、WAL、SHM 和 `known_hosts` 强制 `0600`；
- Windows：ACL 只授予当前用户 SID 和必要系统主体；
- 启动时检查目录和文件所有者；
- 拒绝危险的符号链接、非常规文件和不可信挂载位置；
- 对自定义 `YUKINAL_DATA_DIR` 做规范化和所有权验证；
- 如果审计记录用于取证，增加 HMAC 链、签名或外部只追加存储；
- 是否使用 SQLCipher 应根据离线磁盘攻击威胁模型决定。

### 3.7 安装包签名和发布供应链

项目文档明确说明当前没有 Windows 签名、macOS 公证和更新产物签名：[`packaging.md`](https://github.com/Bad0RANG3/Yukinal/blob/5c551d74caff70887dfa22b2f27a37ce56f9e414/docs/packaging.md#L65-L70)。

对于能够保存服务器配置、启动本地进程、发起 SSH 连接和执行远程命令的软件，这属于发布阻断项。

#### 修复方案

- Windows 使用 Authenticode；
- macOS 使用 Developer ID 并完成 notarization；
- Release 提供 SHA-256 校验和；
- CI 使用受保护环境和短期身份完成签名；
- 生成 SBOM 和构建来源证明；
- 自动更新必须验证签名，不能只依赖 HTTPS；
- 发布流程记录源提交、构建环境和产物哈希。

## 4. 性能与稳定性优化

### 4.1 P0：终端高输出可能令事件转发永久停止

终端广播队列容量为 1024：[`manager.rs`](https://github.com/Bad0RANG3/Yukinal/blob/5c551d74caff70887dfa22b2f27a37ce56f9e414/crates/terminal/src/manager.rs#L66-L75)。

每个 PTY 数据块会被转换成字符串并单独发送：[`manager.rs`](https://github.com/Bad0RANG3/Yukinal/blob/5c551d74caff70887dfa22b2f27a37ce56f9e414/crates/terminal/src/manager.rs#L310-L329)。

桌面转发层对所有接收错误统一执行 `break`，因此一次 `Lagged` 就可能结束整个转发任务：[`lib.rs`](https://github.com/Bad0RANG3/Yukinal/blob/5c551d74caff70887dfa22b2f27a37ce56f9e414/apps/desktop/src-tauri/src/lib.rs#L173-L215)。

#### 修复方案

- 单独匹配 `RecvError::Lagged(n)`，记录丢失并继续接收；
- 向前端发送结构化的“输出丢失”通知；
- 按终端会话隔离队列，防止一个刷屏会话影响其他会话；
- 按 8–16ms 或 32–64KB 合并输出；
- 增加单会话背压和高水位指标；
- 不为每个小数据块执行一次 JSON 序列化和 Tauri IPC。

#### 压力测试

- `yes` 持续输出；
- 大型项目编译日志；
- 多终端并发输出；
- UI 暂停消费后恢复；
- 队列溢出后打开新终端。

### 4.2 P1：Provider SSE 和 Sidecar 帧需要真实上限

Provider SSE 当前不断执行字符串拼接和 `split("\n")`，没有单行及累计字节上限：[`openai-compatible.ts`](https://github.com/Bad0RANG3/Yukinal/blob/5c551d74caff70887dfa22b2f27a37ce56f9e414/apps/agent/src/providers/openai-compatible.ts#L292-L312)。

Sidecar Rust 入口使用 `BufReader.lines()`，需要先读取完整行才知道行有多大：[`sidecar/mod.rs`](https://github.com/Bad0RANG3/Yukinal/blob/5c551d74caff70887dfa22b2f27a37ce56f9e414/crates/core/src/sidecar/mod.rs#L381-L415)。

Rust 向 Sidecar 写入请求时也没有验证统一帧大小：[`sidecar/mod.rs`](https://github.com/Bad0RANG3/Yukinal/blob/5c551d74caff70887dfa22b2f27a37ce56f9e414/crates/core/src/sidecar/mod.rs#L140-L177)。

#### 建议统一限制

| 对象 | 建议上限 |
| --- | --- |
| Sidecar/MCP 单帧 | 8 MiB，读写双向同时执行 |
| SSE 单行 | 1 MiB 或更小 |
| 单次 Provider 累计响应 | 根据最大输出 Token 转换并设置硬上限 |
| `/models` 响应 | 1–4 MiB |
| 单个工具参数 | 256 KiB–1 MiB |
| 单次事件数量 | 明确上限 |
| 单次流持续时间 | 保留总超时和无数据超时 |

超限时应立即取消流并返回结构化错误，不能先把完整数据读入内存再丢弃。

### 4.3 P1：减少附件 Base64 多次复制

当前一条消息允许最多 5 MiB 原始内联附件，Base64 后约为 6.7 MiB：[`chat.ts`](https://github.com/Bad0RANG3/Yukinal/blob/5c551d74caff70887dfa22b2f27a37ce56f9e414/packages/shared/src/types/chat.ts#L126-L138)。

这份数据可能依次存在于：

1. 浏览器 FileReader 和 JavaScript 字符串；
2. Tauri IPC JSON；
3. Rust `String` 和 `serde_json::Value`；
4. Rust 再次序列化后的 Sidecar 帧；
5. Node.js 字符串和消息对象；
6. Provider 请求体。

实际峰值内存可能是原文件的数倍。

#### 修复方案

- 附件先写入权限受控的临时目录；
- IPC 只传内容哈希、元数据和文件句柄；
- Rust 校验 magic bytes、大小及文件权限；
- Sidecar 按需流式读取；
- 只有 Provider 协议要求时才生成 Base64；
- 同一附件按 SHA-256 去重；
- 对话数据库保存引用，不重复保存整段 Base64；
- 任务结束或超过保留期后清理临时文件。

### 4.4 P2：数据库访问模型需要支持长期数据增长

数据库当前是一个同步 SQLite 连接加全局 `Mutex`。部分 async command 会直接调用同步数据库操作，长查询可能占用 Tokio worker：[`database/lib.rs`](https://github.com/Bad0RANG3/Yukinal/blob/5c551d74caff70887dfa22b2f27a37ce56f9e414/crates/database/src/lib.rs#L16-L40)。

聊天搜索使用 `%关键词%`、相关子查询和 `OFFSET`，历史数据增长后会逐步退化：[`chat.rs`](https://github.com/Bad0RANG3/Yukinal/blob/5c551d74caff70887dfa22b2f27a37ce56f9e414/crates/database/src/repositories/chat.rs#L56-L89)。

#### 修复方案

- 使用专用数据库线程或 actor；
- 为读取提供少量连接池，写操作维持单写者；
- 配置 `busy_timeout`；
- 批量写入流式事件和审计记录；
- 聊天搜索改用 SQLite FTS5；
- `OFFSET` 改为 `(updated_at, id)` 游标分页；
- 明确聊天、事件、证据和日志保留策略；
- 数据清理使用小批次后台任务，避免长事务。

#### 基准测试数据集

- 10 万条聊天消息；
- 1 万个会话；
- 100 万条工具执行或活动记录；
- 多个 Agent 同时写入；
- 数据库处于 WAL checkpoint 和清理阶段时执行终端、SSH 和搜索。

### 4.5 P3：后台轮询和前端渲染

MCP 和 Sidecar 使用周期性 `try_wait` 观察进程退出。少量进程时影响有限，但 MCP 数量增长后会产生持续后台唤醒。可由统一进程监督器使用真正的 wait/reaper 任务和状态通知替代。

前端可以继续优化：

- 流式 Token 每动画帧批量更新一次；
- 消息完成后再执行完整 Markdown 解析；
- 历史消息和大型工具输出使用虚拟列表；
- 超长代码块延迟高亮；
- 终端 resize、搜索和输入事件节流；
- 使用选择器缩小状态订阅范围，避免整个工作区重复渲染。

## 5. 修复实施计划

### 第一阶段：安全止血（1 周）

- [ ] 终端禁用自动 TOFU，统一为显式指纹确认；
- [ ] Provider 非本机地址强制 HTTPS；
- [ ] Provider 禁止跨源重定向；
- [ ] Rust Tauri command 增加 Provider URL 二次校验；
- [ ] MCP 使用 `env_clear()` 和最小环境白名单；
- [ ] 终端 `Lagged` 后继续转发，而不是退出；
- [ ] Provider SSE、Sidecar stdout/stderr 增加读取前上限；
- [ ] 添加上述问题的安全回归测试。

### 第二阶段：隔离与可靠性（2–3 周）

- [ ] 实现跨平台 `ProcessSupervisor`；
- [ ] Unix 进程组和 Windows Job Object；
- [ ] 数据目录权限、所有者和符号链接检查；
- [ ] Markdown 图片私网地址拦截和 Rust 代理下载；
- [ ] 终端事件合并、单会话背压和丢失通知；
- [ ] 附件从 Base64 IPC 改为临时文件/句柄模式；
- [ ] 加入终端刷屏、恶意 SSE、超长帧和孙进程测试。

### 第三阶段：成熟发布（1–2 个月）

- [ ] MCP 文件、网络和环境权限沙箱；
- [ ] 禁止未锁版本的运行时包下载；
- [ ] Windows 和 macOS 产物签名、公证；
- [ ] SBOM、校验和和构建来源证明；
- [ ] 自动更新签名验证；
- [ ] 数据库 FTS5、游标分页和专用执行线程；
- [ ] 三平台安装后启动及升级测试；
- [ ] 建立稳定的性能基准和回归阈值。

## 6. 发布前安全门禁

建议将以下内容加入 CI：

```text
格式化和静态检查
  ├─ TypeScript lint/typecheck
  ├─ cargo fmt --check
  └─ cargo clippy -- -D warnings

依赖与秘密检查
  ├─ pnpm audit --prod
  ├─ cargo audit 或 cargo deny
  ├─ 秘密扫描
  └─ 许可证策略检查

安全回归
  ├─ SSH 未知/变化指纹
  ├─ MCP 环境变量隔离
  ├─ MCP 孙进程回收
  ├─ Provider HTTP/redirect/DNS 策略
  ├─ SSE 和 NDJSON 超限
  └─ Markdown 私网图片

压力与耐久性
  ├─ 终端持续高输出
  ├─ 多会话并发
  ├─ 大型附件
  ├─ 十万级消息搜索
  └─ 长时间 Agent 运行

发布验证
  ├─ 三平台安装和启动
  ├─ 签名、公证和校验和验证
  ├─ 干净环境升级/降级
  └─ SBOM 与提交、产物对应
```

## 7. 当前已有的安全优势

以下设计值得保留并继续统一：

- 密钥进入系统凭据库，SQLite 主要保存引用；
- Markdown 不使用 `dangerouslySetInnerHTML`；
- 外部链接限制协议并交给系统浏览器；
- MCP HTTP 对 HTTPS、重定向和帧大小已有较强约束；
- stdio MCP 协议帧使用有界读取；
- SSH 已有指纹固定、Host CA 和 KRL 基础设施；
- 文件写入具备备份、原子替换和大小限制；
- 日志具备凭据脱敏和长度限制；
- Tauri 前端没有直接 shell/spawn 权限；
- 附件已经设置数量和大小上限。

本次检查结果：

- `pnpm audit --prod --audit-level high`：未发现已知生产依赖漏洞；
- 仓库秘密扫描：通过，共检查 581 个文件；
- Rust 依赖审计：当前检查环境没有 `cargo`/`cargo-audit`，因此不能据此判断 Rust 依赖不存在漏洞。

## 8. 最终评价

从代码工程角度，Yukinal 已经不是普通的界面 Demo，而是具备明确边界设计和跨层工程结构的复杂桌面项目。它的主要不足不是“完全没有安全能力”，而是部分关键入口尚未统一应用已有的安全模型，同时本地第三方进程、网络流量和高吞吐 IPC 仍缺少成熟产品应有的强制资源边界。

比较准确的阶段定位是：

> 安全边界设计较完整、工程复杂度较高，但关键系统风险尚未完全收口的准生产级个人项目。

当 SSH 首连、MCP 隔离、Provider 出站策略、流量上限、终端背压和发布签名全部完成后，才适合进一步定位为可长期使用的成熟开源运维工作区。

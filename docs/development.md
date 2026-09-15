# 开始开发

前置条件：

- Node.js `>= 24`（根 `package.json` 的 `engines`，同时也是 esbuild 的 `--target` 与安装包对用户的要求）
- pnpm `11.8.0`（`packageManager` 固定，建议用 Corepack 启用）
- Rust `1.85` 或更高版本，工具链 `stable` 并包含 `rustfmt` 与 `clippy`（`rust-toolchain.toml`）
- 目标平台运行 Tauri 2 所需的系统依赖（Linux 需要 webkit2gtk 等，见 `.github/workflows/check.yml`）

安装依赖并跑完整校验：

```bash
pnpm install
pnpm check
```

启动浏览器里的界面预览（不需要 Rust）：

```bash
pnpm desktop:dev
# 等价于 pnpm --filter @yukinal/desktop dev
```

预览地址固定为 `http://127.0.0.1:1420/`（Vite 配置了 `strictPort`）。它适合调界面，但原生能力不可用。

启动完整桌面应用：

```bash
pnpm --filter @yukinal/desktop tauri dev
```

该命令会先构建 Agent sidecar 产物，再启动 Vite，然后打开 Tauri 窗口。窗口启动时 Rust 会自动拉起 sidecar（与 `agent_spawn` 命令走同一条启动路径），因此 Agent 面板应该立即处于可用状态。

单独运行 Agent sidecar（调试协议时使用，stdout 上是 NDJSON 帧）：

```bash
pnpm agent:dev
# 等价于 pnpm --filter @yukinal/agent dev（tsx watch src/index.ts）
```

清理构建产物（不会碰数据库和凭据库）：

```bash
pnpm clean
```

首次使用的顺序：在「设置 ▸ Provider」里选协议（OpenAI-compatible / Anthropic / Gemini）、填写端点、模型和 API key（本地端点可以留空 key），再到「服务器」里添加一台服务器（需要用户名，以及密码、私钥（带口令的私钥请一并填口令）、OpenSSH 用户证书或 ssh-agent），首次连接会按指纹策略处理服务器身份，连接后即可使用概览、终端、文件、服务与日志。

### 首次使用引导

首次打开会显示三步引导，也可从窗口顶部「使用引导」重新打开：

1. 选择或保存模型配置，点击「测试模型连接」。测试通过已保存的凭据发送一条简短消息，验证实际文本回复；可能产生少量模型费用，超时或失败可重试。
2. 添加或选择服务器，点击「连接并验证 SSH」。连接成功后继续；失败可编辑地址与认证信息后重试。首次连接采用 TOFU，应提前独立核验主机指纹。
3. 点击「填入首次排查任务」，将只读巡检草稿放入 Agent 面板，并设置「操作前询问」。用户检查后发送；已有草稿或正在运行的任务不会被覆盖。

「稍后设置」会记住跳过状态。浏览器预览可查看引导，但不能测试模型或连接 SSH。

### 验证命令

`pnpm check` 是唯一的本地门禁（`scripts/check.mjs`），CI 也是跑同一条命令（`.github/workflows/check.yml`，三个平台各跑一遍）。它按固定顺序执行，遇到必需步骤失败即停止：

1. `node scripts/check-publication.mjs` —— 公开文档卫生（禁止引用未发布的内部材料；公开标准的编号是例外，见脚本里的 `PUBLIC_STANDARD`）
2. `node scripts/check-secrets.mjs` —— 已跟踪文件中不得出现凭据形态的字符串
3. 构建契约库：`@yukinal/shared`、`@yukinal/provider-sdk`、`@yukinal/agent-sdk`
4. `pnpm -r --if-present typecheck` —— 全工作区类型检查
5. 构建 Agent：先 `tsc -p tsconfig.build.json --noEmit` 类型检查，再用 esbuild 打成**单个**自包含 ESM 文件 `apps/agent/dist/index.js`
6. `node scripts/check-packaging.mjs` —— 打包契约：`bundle.resources` 的目标位置与 `tauri.conf.json`、图标、`beforeBuildCommand` 顺序、esbuild 目标与 `engines.node` 一致，以及「安装后的 agent 目录里恰好两个文件：bundle 与声明 ESM 的 `agent/package.json`」
7. `pnpm -r --if-present test` —— 所有工作区单元测试
8. 构建桌面端：`pnpm --filter @yukinal/desktop build`（Vite）
9. `node scripts/smoke-sidecar.mjs` —— 用真实 stdio 传输启动 sidecar（跑的是构建出来的 bundle，不是 `tsx src/index.ts`），断言握手、ping、工具列表、方法可用性、不完整请求返回 `INVALID_PARAMS`、坏帧不会杀死进程、父进程关闭 stdin 后干净退出
10. `node scripts/smoke-packaged-agent.mjs` —— 把 bundle 按 `bundle.resources` 声明的位置摆进一个临时目录（没有 `node_modules`、没有 `package.json`），再跑一遍同样的冒烟：这是「安装之后到底能不能起来」唯一能在这里验证的部分
11. 若 `cargo` 在 PATH 上：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo check --workspace --all-targets`、`cargo test --workspace -- --test-threads=1`（最后一项会带上 `YUKINAL_TEST_NODE` 与 `YUKINAL_TEST_ENTRY`，让跨语言集成测试启动真实的 sidecar 产物，且要求该产物必须存在）

分层执行（需要缩小范围时用）：

```bash
pnpm typecheck                     # 全工作区类型检查
pnpm test                          # 全工作区单元测试
pnpm build:libs                    # 只构建 packages/**
pnpm smoke:sidecar                 # 只跑 sidecar stdio 冒烟
pnpm package                       # 构建安装包（契约库 → agent bundle → 打包契约 → tauri build）
cargo fmt --all --check            # Rust 格式
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -- --test-threads=1
```

Windows 上还可以手动验证「窗口真的被创建出来」（需要先构建出 `target/debug/yukinal-desktop.exe`）：

```powershell
pwsh -File scripts/check-desktop-window.ps1
```

# 变更记录

本文件记录对用户有意义的版本变化和交付状态。详细实现与历史讨论可通过 Git 记录追溯。

## 未发布

- 新增进程内 SSH 回环协议验收，验证主机密钥 pin、命令 stdout/stderr 与非零退出码、输出限额、超时和取消；隔离临时目录的真实 SFTP 子系统验收除底层读写外，还通过 `RemoteFileService` 覆盖备份碰撞防覆盖、多链接/未知链接数拒绝、revision 守卫编辑、过期恢复拒绝、有效恢复与校验后清理。替换前复核增加内容 SHA-256；故障注入将目标改成相同长度并恢复原 mtime，确认 rename 前仍以 `ConcurrentChange` 拒绝、并发内容保留且 staging 临时文件清除。SFTP 仍不是 CAS，最后一次内容复核后到 rename 的窗口仍存在。它们补足协议和文件服务组合证据，但不替代 OpenSSH 兼容性、真实目标机、桌面 IPC/AppState 或 UI 业务路径验收。
- 重新梳理产品定位、三条核心任务、安全边界和发布验收标准。
- 将固定版本且按 SHA-256 校验的 Node.js 运行时纳入桌面安装包；本机 Windows NSIS/MSI 构建成功，当前 MSI 管理员映像载荷启动/关闭冒烟通过。针对早先 NSIS 卸载曾删除数据目录的风险，明确维持产品规则：卸载永远保留数据库、设置和凭据；即使 Tauri 确认页的删数复选框被勾选，hook 也会阻止递归删除并在交互卸载时解释如何卸载后手动清理，静默与升级路径同样保留。
- 补强 NSIS 卸载回归保护：静态检查锁定 hook 存在、用户勾选后的说明提示、复选框状态清零、Roaming/LocalAppData 两条且仅两条删除语句仍受保护 hook 与非更新条件约束，并拒绝缺失状态捕获和额外无条件删除。Windows UI smoke 从 Tauri 生成脚本提取真实回调，操作原生 checkbox，验证默认未选、勾选提示、勾选与未勾选时 canary 均保留，更新模式也保留；另在 `/S` 静默模式通过测试专用参数预置“已勾选”状态，确认卸载正常退出且 canary 保留。所有破坏性验证只发生在临时 fixture。真实 native checkbox UI smoke 已在 GitHub Windows package run `36667041840` 通过；同一 run 也通过实际 `/UPDATE` 安装器重跑和数据保留断言。
- 扩展 NSIS 安装生命周期冒烟：在隔离目录真实安装后，以 `/UPDATE` 对同一安装执行就地更新，先移除再核实安装包恢复 `NOTICE` 文件且 SHA-256 与更新前一致，并验证 Roaming/LocalAppData canary 跨更新和卸载均未改变；GitHub Windows package run `36667041840` 已通过此更新生命周期、manifest 校验和产物上传。
- 修正两个 macOS Node.js `.tar.gz` 校验值与归档名错配的问题；打包前新增六平台官方 SHASUMS 全量校验，当前六个 pin 均已核对通过，Windows 打包前置流程与 Tauri release 编译（`--no-bundle`）通过。
- 为跨平台安装包产物生成 SHA-256 与构建来源清单；CI 上传前复核，并由门禁确保上传规则覆盖所有清单支持的安装包后缀。
- 新增受限的 MSI 安装/冒烟/卸载验收脚本和 NSIS 生命周期脚本，并在安装前拒绝触碰既有 Yukinal 数据目录。GitHub Windows package run `36667041840` 已通过 MSI 与 NSIS 的真实隔离安装、启动、关闭和静默卸载/数据保留步骤；runner 本身装有 Node，因此应用侧以受限 PATH 验证随包运行时，但仍缺整机完全未安装 Node 的复验。
- 扩展 Windows 包冒烟支持在 Agent 就绪后强制终止应用父进程；最新 MSI 提取载荷的正常关窗 1 次、强制终止 3 次均未遗留 sidecar。
- 为可选真实 Provider 验收补上 OpenAI 兼容 Chat Completions 与 Responses 两种 live 测试入口；真实服务调用仍待测试账号和端点。
- 补上 Host RPC 连接关闭时对全部挂起请求的拒绝与在途工具取消回归；交付文档现区分已自动覆盖的故障分支与仍需真实远端/完整 UI 环境验收的边界。
- 本机 `pnpm check` 全套门禁通过；PR #2 安装器代码提交 `a3dbb71` 的三平台代码检查、依赖审计及 Windows package run `36667041840` 全部成功，Windows 包含实际 MSI/NSIS 安装器生命周期和 NSIS `/UPDATE` 数据/文件恢复检查。整机无 Node 的安装验收、macOS/Linux 实际安装、真实 SSH/Provider/MCP 场景仍待完成。
- 三条核心用户路径的可重置真实环境端到端验证仍待完成。

## 1.0.0 — 2026-09-13

- 建立 Tauri/Rust 宿主、React 工作区、Node.js Agent sidecar 与共享 IPC 契约的版本基线。
- 提供 SSH 服务器管理、健康快照、终端、远程文件、服务与日志、Agent 对话和受控调查任务的实现。
- 建立权限审批、持久任务计划、活动记录、凭据隔离和本地/回环测试覆盖。

`1.0.0` 是代码接口基线；安装包仍未完成三平台真实安装验收。当前交付条件见[交付与发布](./release.md)。

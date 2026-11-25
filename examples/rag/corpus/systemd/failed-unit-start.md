+++
title = "systemd 单元启动失败"
source = "https://www.freedesktop.org/software/systemd/man/latest/systemctl.html"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_app01", "srv_app02"]
acl = ["public"]
+++

# systemd 单元启动失败

## 快速收集状态 {#status-triage}

单元进入 `failed` 时，先记录单元名、启动时间、退出码和失败原因，不要反复执行 `restart` 覆盖证据。

```bash
systemctl status <unit> --no-pager -l
systemctl show <unit> -p LoadState,ActiveState,SubState,Result,ExecMainCode,ExecMainStatus,FragmentPath,DropInPaths,NRestarts
journalctl -u <unit> -b --no-pager -n 200
```

`Result` 可能是 `exit-code`、`signal`、`timeout`、`resources` 或 `start-limit-hit`。`ExecMainStatus` 是主进程最后状态；脚本包装器会让真实退出码和实际根因分离，因此还要看完整日志和脚本内部错误处理。

## 读取退出状态 {#exit-status}

- `exit-code` 且状态非零：应用参数、配置、权限或依赖失败。
- `signal`：进程被信号终止，结合 `SIGKILL`、OOM 和人工操作时间线判断。
- `timeout`：`TimeoutStartSec` 或 `TimeoutStopSec` 到期，检查启动阻塞和停止握手。
- `resources`：cgroup 资源限制或 OOM 触发。
- `start-limit-hit`：短时间内重复启动超过限制，应先修复根因再处理启动限制。
- `exec-format-error`、`noexec`、`203/EXEC`：可执行文件路径、权限、挂载选项或解释器缺失。

## 常见原因 {#common-causes}

- `ExecStart` 使用相对路径、错误二进制或不存在的工作目录。
- `User`/`Group` 无权读取配置、证书、密钥或写运行时目录。
- `EnvironmentFile` 缺失、格式错误或变量没有展开。
- 依赖的数据库、网络或挂载尚未就绪，单元没有正确的 `After`/`Wants`。
- 前台进程自行 daemonize，导致 systemd 认为主进程退出。
- 启动超时过短、memory/pids 限制过低或 OOM。
- 单元文件语法错误，或修改后没有执行 `systemctl daemon-reload`。

## 修复与验证 {#remediation}

先用 `systemd-analyze verify <unit>` 检查明显的单元语法问题，再用服务账户或受控环境手动执行等价命令，确认环境和权限。修复单元配置后执行 `systemctl daemon-reload`，再启动测试实例并观察 `systemctl status`、`journalctl` 和实际依赖。验证正常启动、依赖暂时不可用、优雅停止和重启四条路径；不要只用 `reset-failed` 清除状态就认为问题解决。

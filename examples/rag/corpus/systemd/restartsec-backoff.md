+++
title = "systemd RestartSec 与启动频率限制"
source = "https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_app01", "srv_app02"]
acl = ["public"]
+++

# systemd RestartSec 与启动频率限制

## 重试策略的组成 {#restart-policy}

`Restart=` 决定服务在退出、失败或超时后是否重启，`RestartSec=` 决定两次启动之间等待多久。常见的 `on-failure` 不会在所有主动停止场景重试，而 `always` 可能让配置错误变成快速循环。`RestartSec` 不是修复手段，只是给依赖恢复和日志收集留下窗口；如果应用每次启动都立即退出，增加等待只会降低循环频率。

## 查看生效值 {#effective-policy}

```bash
systemctl show <unit> -p Restart -p RestartUSec -p NRestarts \
  -p StartLimitIntervalUSec -p StartLimitBurst
systemctl cat <unit>
journalctl -u <unit> -b --no-pager -n 200
```

`RestartUSec` 是实际生效的等待时间。检查 drop-in 是否覆盖了主单元文件，避免只看 `/lib/systemd/system` 中的默认值。修改单元后需要 `daemon-reload`；`reset-failed` 只清除失败计数，不会让错误配置变正确。

## 启动频率限制 {#rate-limit}

`StartLimitIntervalSec=` 和 `StartLimitBurst=` 通常位于 `[Unit]`，用于限制单位时间内的启动次数。消息 `start request repeated too quickly` 或 `start-limit-hit` 表示服务在短时间内反复启动，systemd 已停止继续尝试。先收集每次退出的原因，再决定调整限制。盲目提高 burst 会导致不断重启、日志膨胀和依赖过载。

## 验证 {#verification}

用可控故障场景验证策略：正常退出、非零退出、信号终止、依赖不可用和配置错误。观察 `NRestarts`、启动时间间隔和最后状态，确认不会形成快速循环，也不会在短暂依赖故障后永久放弃。若设置较长 `RestartSec`，确认健康检查和告警能覆盖这段空窗；若设置较短，确认日志和重启次数不会淹没真正的根因。

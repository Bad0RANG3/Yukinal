+++
title = "systemd timer 未按计划运行"
source = "https://www.freedesktop.org/software/systemd/man/latest/systemd.timer.html"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_app01", "srv_app02"]
acl = ["public"]
+++

# systemd timer 未按计划运行

## 列出定时器与状态 {#timer-inspection}

```bash
systemctl list-timers --all --no-pager
systemctl status <name>.timer <name>.service --no-pager -l
systemctl cat <name>.timer
```

查看 `NEXT`、`LAST`、`LEFT`、`ACTIVE` 和 `UNIT`。timer 名称通常与同名 service 绑定，但也可以在 `Unit=` 中指定其他服务。timer 未列出或 `inactive` 时，检查是否执行过 `enable`，以及它是否被 mask、条件判断跳过或启动失败。

## 日历表达式与持久性 {#calendar-and-persistence}

`OnCalendar=` 使用 systemd 的时间表达式，不是 cron 语法。用 `systemd-analyze calendar '<expression>'` 预览下次触发和时区依赖。`Persistent=true` 可在机器关机期间错过触发后补跑一次，但不会补跑所有历史实例；`RandomizedDelaySec=` 会故意分散负载，`AccuracySec=` 会允许触发误差。若表达式中的时间和主机时区不一致，也会表现为“延迟”或“没跑”。

```bash
systemd-analyze calendar '*-*-* 02:30:00' --iterations=3
systemctl show <name>.timer -p NextElapseUSecRealtime,LastTriggerUSec,Unit,Persistent,AccuracyUSec,RandomizedDelayUSec
```

## 关联 service 与权限 {#service-binding}

即使 timer 正常触发，关联 service 也可能因退出码、权限、运行目录或依赖失败而没有完成工作。检查 `journalctl -u <name>.service -u <name>.timer`，确认实际启动时间、执行用户、退出码和任务日志。不要只看 timer 的 `ACTIVE`，那只能说明计时器存在。

## 验证 {#verification}

先手动启动 service 验证任务本身，再用 `systemctl start <name>.timer` 或临时短周期表达式在测试环境验证触发。检查 `NEXT` 是否更新、service 是否成功、任务产物和告警状态。验证完成后恢复正式表达式并执行 `daemon-reload`；如果使用 `Persistent=true`，还要测试一次受控的重启或暂停场景，确认补跑符合业务预期。

+++
title = "systemd journald 存储与保留"
source = "https://www.freedesktop.org/software/systemd/man/latest/journald.conf.html"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_app01", "srv_app02"]
acl = ["public"]
+++

# systemd journald 存储与保留

## 先看实际占用 {#storage-accounting}

日志盘告警时，先区分 journal、应用日志、Docker 日志和其他大文件。journald 占用可以用专用命令查看，不要直接删除 `/var/log/journal` 中的文件。

```bash
journalctl --disk-usage
df -hT /var/log/journal /run/log/journal 2>/dev/null
```

## 持久化与易失存储 {#storage-mode}

`Storage=persistent` 通常把日志写入 `/var/log/journal`，`volatile` 写入 `/run`，重启后丢失；`auto` 的行为取决于目录和配置。容器或只读根文件系统中，journald 的实际位置可能与宿主机不同。先确认服务运行位置和挂载，再谈保留策略。

## 设置保留上限 {#retention}

在 `/etc/systemd/journald.conf.d/` 中放入受审查的 drop-in，常用键包括 `SystemMaxUse`、`SystemKeepFree`、`SystemMaxFileSize`、`MaxRetentionSec` 和 `RuntimeMaxUse`。值要结合磁盘容量、审计要求和故障取证窗口，不要简单设置为极小值。

```ini
[Journal]
SystemMaxUse=4G
SystemKeepFree=8G
SystemMaxFileSize=256M
MaxRetentionSec=30day
```

修改后执行 `systemctl restart systemd-journald` 并检查服务状态与配置警告。重启 journald 不应导致业务服务重启，但应先确认日志采集链路可接受短暂中断。

## 清理与验证 {#cleanup}

需要立即回收空间时，使用 journald 提供的 vacuum 操作，并在执行前保留必要证据：

```bash
journalctl --vacuum-size=2G
journalctl --vacuum-time=14d
journalctl --disk-usage
```

确认旧日志按策略减少、新日志仍可写入、审计留存的窗口符合要求。持续增长时，检查日志级别、重复错误和日志风暴；只清理文件不解决根因，下一次故障可能再次填满磁盘。

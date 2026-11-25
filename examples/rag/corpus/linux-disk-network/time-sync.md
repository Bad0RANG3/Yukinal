+++
title = "Linux 时间同步与时钟偏差"
source = "https://www.freedesktop.org/software/systemd/man/latest/timedatectl.html"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_linux01", "srv_linux02"]
acl = ["public"]
+++

# Linux 时间同步与时钟偏差

## 时钟状态 {#clock-status}

```bash
timedatectl
date -Ins
timedatectl show -p NTPSynchronized -p TimeUSec -p RTCUSec
```

`NTPSynchronized=yes` 表示 systemd 认为已同步，不等于每个应用都使用了同一时钟源。区分 UTC、本地时区和 RTC：时区设置错误会让日志显示偏移，但不一定代表时钟不准；RTC 与系统时钟的关系还要结合虚拟化平台和重启行为判断。

## 时间源与网络 {#time-sources}

使用 chrony 时检查 `chronyc tracking`、`chronyc sources -v` 和 `chronyc sourcestats -v`；使用 systemd-timesyncd 时检查 `systemctl status systemd-timesyncd`、`/etc/systemd/timesyncd.conf` 和 `timedatectl timesync-status`。同一主机原则上只运行一个时间同步服务，避免多个服务争用时钟。

检查 UDP 123 是否可达、DNS 能否解析 NTP 源、上游服务器是否响应，以及防火墙是否只限制了客户端回包。云主机还要确认虚拟化时间同步与客户机内 NTP 的优先级。时间偏差可能表现为证书校验失败、Kerberos 认证失败、日志顺序异常、任务重复或分布式锁异常。

## 同步配置 {#sync-configuration}

选择经过审查的内部或公共时间源，配置合理的源数量和网络路径。不要为了测试随意修改系统时钟；在分布式系统中，向前或向后跳变可能破坏数据库、证书和任务调度。若必须手动校正且偏差较大，应先评估应用对时间跳变的容忍度，并由有权限的操作者在维护窗口执行。

## 验证 {#sync-validation}

等待若干个同步周期，观察 `NTPSynchronized`、chrony 的偏移和源状态是否稳定。用两个时间敏感客户端验证 TLS 或认证流程，再检查应用日志中的时间排序。确认重启后仍能同步，且防火墙、DNS 和上游时间源至少各有一个健康检查。记录时间源、时区、观察窗口和回滚配置；单次 `date` 正确不能证明长期同步稳定。

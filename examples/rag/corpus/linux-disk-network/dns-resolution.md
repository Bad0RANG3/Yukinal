+++
title = "Linux DNS 解析故障"
source = "https://man7.org/linux/man-pages/man5/resolv.conf.5.html"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_linux01", "srv_linux02"]
acl = ["public"]
+++

# Linux DNS 解析故障

## 解析路径 {#resolution-path}

Linux 解析通常由 `nsswitch.conf` 决定是否查询 `files`、`dns`、`resolve` 等后端，再由 `resolv.conf` 或 systemd-resolved 决定上游服务器、search domain 和超时。容器还会使用容器运行时注入的 DNS 配置。先确认进程所在网络命名空间中的配置，不要只在宿主机 shell 里测试。

```bash
cat /etc/nsswitch.conf
cat /etc/resolv.conf
resolvectl status 2>/dev/null || true
getent hosts <name>
```

## 分层诊断 {#dns-diagnosis}

- `getent hosts` 验证系统实际解析路径；直接用 `dig` 可能绕过 NSS，结果不一致时不能误判。
- 检查 search domain 是否让短名称拼接到错误后缀，以及 `ndots` 是否导致先查绝对域。
- 检查上游 DNS 是否可达，UDP 53 是否被放行，以及大响应或 DNSSEC 失败时 TCP 53 是否可用。
- 检查 systemd-resolved 的 per-link DNS、VPN、容器网络和 split-horizon 配置。
- 检查 `/etc/hosts`、缓存、TTL 和权威记录是否已更新。
- 检查时间是否严重偏差；DNSSEC、TLS 和认证系统可能因此失败。

```bash
getent ahosts <name>
resolvectl query <name> 2>/dev/null || true
getent hosts <known-good-name>
```

## 常见故障 {#common-failures}

错误的上游 DNS、被防火墙静默丢包、容器与宿主机 DNS 不一致、search domain 污染、过期缓存、DNSSEC 验证失败和本地 `/etc/hosts` 覆盖都可能导致解析失败。不要直接覆盖由 NetworkManager 或 systemd-resolved 管理的 `resolv.conf`，否则重启或网络切换后会再次改变。

## 修复与验证 {#verification}

修正配置后，用应用的实际运行用户和网络命名空间验证系统解析，再验证业务连接。确认短名、完整域名、内部域名和外部域名各有一条成功与失败边界，检查 DNS 超时是否落在应用重试预算内。记录变更的 DNS 源、search domain 和回滚值；如果问题只在某个网络或容器出现，继续比较该命名空间的配置，而不是修改全局 DNS。

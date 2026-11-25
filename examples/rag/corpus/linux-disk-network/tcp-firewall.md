+++
title = "Linux TCP 连接与防火墙排查"
source = "https://man7.org/linux/man-pages/man8/ss.8.html"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_linux01", "srv_linux02"]
acl = ["public"]
+++

# Linux TCP 连接与防火墙排查

## 先判断连接到哪一步 {#tcp-path}

先明确方向：本机访问远端、外部访问本机，还是容器经过宿主机转发。`Connection refused` 通常表示收到 RST，可能是端口未监听或主动拒绝；连接超时更可能是丢包、路由或防火墙静默丢弃；`No route to host` 则偏链路或路由问题。不要把所有连接失败都归因于防火墙。

```bash
ss -lntp
ss -tnp
ip addr
ip route
```

`ss -lntp` 显示监听地址和端口。进程只监听 `127.0.0.1` 时，远端无法连接；服务监听 `0.0.0.0` 也可能被上层防火墙拦截。`ss -tn state syn-sent` 可以显示正在等待握手的目标，`state established` 能确认是否已建立连接。

## 本机监听与连接 {#local-sockets}

在上游主机验证回环接口，再从允许的客户端测试网络路径：

```bash
curl -sv --max-time 3 http://127.0.0.1:<port>/health
curl -sv --max-time 3 http://<host>:<port>/health
ss -tn state established '( dport = :<port> or sport = :<port> )'
```

`curl` 受代理环境变量影响，必要时先确认代理设置；测试命令应使用明确的受控地址。检查应用是否绑定到 IPv6 地址而客户端只尝试 IPv4，或反之。对容器端口，进一步检查发布端口绑定和 Docker 的 NAT 链。

## 防火墙与安全组 {#firewall-check}

本地规则应使用只读命令查看：

```bash
nft list ruleset 2>/dev/null || iptables -S
iptables -t nat -S 2>/dev/null || true
```

同时检查云安全组、网络 ACL、上游主机防火墙和中间设备。规则顺序、默认策略、连接跟踪、源地址范围和协议端口都必须匹配。不要通过清空规则或临时开放所有端口来证明问题，这会扩大暴露面。对已建立连接的故障，检查 NAT/conntrack 和中间设备空闲超时；对握手失败，检查 SYN 是否到达以及是否有回包。

## 验证 {#verification}

从同机、同网段和允许的外部位置分别验证，记录源地址、目的地址、端口、协议和时间。确认监听、路由、防火墙和上游应用每一层都有对应证据。修复后检查新连接成功、旧连接状态、超时和错误率，并保留规则备份与回滚方式；若只是放宽了防火墙，必须确认没有把数据库或管理端口暴露给不必要来源。

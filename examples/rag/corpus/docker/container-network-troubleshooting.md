+++
title = "Docker 容器网络路径排查"
source = "https://docs.docker.com/engine/network/"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_docker01", "srv_docker02"]
acl = ["public"]
+++

# Docker 容器网络路径排查

## 分层检查 {#network-layers}

容器网络故障通常落在四层路径上：容器内进程是否监听、容器 DNS 是否能解析、容器网络是否互通、宿主机发布端口和防火墙是否放行。先确认请求方向是“外部访问容器”“容器访问容器”还是“容器访问外部服务”，不要在同一层反复重启容器。

```bash
docker inspect --format 'networks={{json .NetworkSettings.Networks}} ports={{json .NetworkSettings.Ports}}' <container>
docker port <container>
docker network inspect <network>
```

## 容器内解析与监听 {#container-checks}

进入受控测试容器或在应用容器中执行只读检查：

```bash
getent hosts <service-name>
ss -lntp
curl -sv --max-time 3 http://127.0.0.1:<port>/health
```

确认进程监听的是 `0.0.0.0` 还是 `127.0.0.1`；若只监听回环地址，其他容器无法访问。使用自定义 bridge 网络时，服务名解析依赖网络别名和容器加入的网络。默认 bridge 网络不提供相同的内置服务名解析，跨网络通信需要明确连接。

## 主机端口与防火墙 {#host-path}

发布端口时要确认绑定地址。`-p 127.0.0.1:8080:80` 只允许本机访问，不适合外部客户端；`-p 8080:80` 的行为还受主机防火墙和云安全组影响。检查宿主机监听、Docker NAT 链和系统防火墙：

```bash
ss -lntp
nft list ruleset 2>/dev/null || iptables -S
docker info --format 'iptables={{.IPTables}}'
```

不要在未备份和未确认规则的情况下清空 `iptables` 或 `DOCKER-USER` 链。容器到外部的失败还要检查宿主机路由、DNS、代理和出口策略；容器之间失败则检查网络成员关系、别名、协议和端口。

## 修复与验证 {#verification}

先用最小请求验证 DNS、TCP 连接和 HTTP 三层，再检查应用层路径、Host 头和 TLS。修复后分别从宿主机、同网络容器和允许的外部位置验证，确认未把管理端口意外暴露到公网。保留变更前后的网络配置、端口绑定和防火墙规则摘要，避免把一次端口改动误判为应用故障。

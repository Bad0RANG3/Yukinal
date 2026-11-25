+++
title = "Nginx 502：上游连接被拒绝"
source = "https://nginx.org/en/docs/http/ngx_http_upstream_module.html"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_nginx01", "srv_nginx02"]
acl = ["public"]
+++

# Nginx 502：上游连接被拒绝

## 现象 {#symptoms}

`error.log` 出现 `connect() failed (111: Connection refused) while connecting to upstream`，客户端收到 502。该错误发生在建立 TCP 连接阶段，通常说明目标地址可达但端口没有监听、监听地址不匹配，或者中间策略直接拒绝了连接。它与上游超时的含义不同。

## 与超时区分 {#connection-refused}

- `Connection refused`：Nginx 收到 TCP RST，问题通常是端口未监听、进程刚退出或目标地址错误。
- `Connection timed out`：握手没有完成，更偏向网络路径、丢包或防火墙静默丢弃。
- `No route to host`：路由或链路层不可达，应优先检查网络和主机状态。
- `Name or service not known`：上游域名解析失败，应检查 DNS 和 Nginx 的 resolver 配置。

确认日志中的 `upstream` 地址、端口以及 `proxy_pass` 实际拼接结果。带变量的 `proxy_pass` 与固定 URI 的路径拼接规则不同，错误拼接可能导致请求打到错误端口。

## 检查监听、地址和网络路径 {#network-path}

在上游主机上执行只读检查：

```bash
ss -lntp
systemctl status <upstream-service> --no-pager
curl -sv --max-time 3 http://127.0.0.1:<port>/health
```

确认应用监听的是 `0.0.0.0`、指定内网地址还是仅 `127.0.0.1`。如果只监听回环地址，Nginx 从另一台主机连接必然被拒绝。再检查上游端口、Nginx `upstream` 配置、主机防火墙和云安全组。不要用 `curl` 访问未知的外部地址；在受控环境内验证 TCP 端口即可。

## 修复与验证 {#remediation}

修复顺序通常是：恢复上游进程或修正端口，调整监听地址，确认 Nginx 和后端处于同一网络路径，最后才考虑健康检查和故障转移。变更后先执行 `nginx -t`，再对单个测试请求观察 `$upstream_addr` 与 `$upstream_status`。如果上游依赖数据库或初始化资源，必须等待其健康检查通过，否则会再次进入 502 循环。

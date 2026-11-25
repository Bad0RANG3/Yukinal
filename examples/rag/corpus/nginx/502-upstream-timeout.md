+++
title = "Nginx 502：上游超时"
source = "https://nginx.org/en/docs/http/ngx_http_proxy_module.html"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_nginx01", "srv_nginx02"]
acl = ["public"]
+++

# Nginx 502：上游超时

## 现象与先决条件 {#symptoms}

客户端收到 `502 Bad Gateway`，Nginx `error.log` 出现类似 `upstream timed out (110: Connection timed out)` 的记录。502 表示反向代理没有从上游拿到可接受的响应，不等于上游业务一定崩溃。先确认请求经过的是哪一层 Nginx，并保留请求时间、Host、上游地址和 `X-Request-ID`，避免只看访问日志中最终状态码。

## 判定是否为上游超时 {#upstream-timeout}

在 `error.log` 中查找 `upstream timed out`、`Connection timed out`、`upstream prematurely closed connection`。在访问日志中临时确认 `$upstream_addr`、`$upstream_status`、`$upstream_connect_time`、`$upstream_response_time` 和 `$request_time`。连接时间很短但响应时间接近超时值，通常说明 TCP 已建立而上游迟迟不返回；连接时间本身接近超时值，则更偏向网络或上游拒绝连接。

```nginx
log_format upstream_probe '$remote_addr "$request" '
    'status=$status upstream=$upstream_addr '
    'connect=$upstream_connect_time response=$upstream_response_time '
    'request_time=$request_time request_id=$request_id';
```

## 常见根因 {#root-causes}

- 上游应用线程池、数据库连接池或任务队列耗尽，请求已经进入应用但长时间没有响应。
- 上游只完成 TCP 三次握手，没有返回 HTTP 响应头或响应体。
- Nginx 的 `proxy_read_timeout` 短于业务正常处理时间，尤其是同步导出和批量查询。
- 上游实例被错误地保持为可用，健康检查没有覆盖真实接口。
- 主机网络、云安全组或中间设备丢弃长连接数据包。

不要仅通过调大超时来“修复”持续堆积的上游；需要同时观察上游 CPU、内存、线程、连接池和错误日志。

## 修复顺序 {#remediation}

1. 先确认上游接口自身在相同超时窗口内是否可完成，必要时用受控测试请求复现。
2. 修复上游慢查询、锁等待、线程饥饿或部署异常。
3. 为读、写和连接设置明确且可解释的 `proxy_connect_timeout`、`proxy_send_timeout`、`proxy_read_timeout`，不要无限放大。
4. 对非幂等请求限制 `proxy_next_upstream` 重试，避免一次用户操作被执行多次。
5. 如果业务必须异步化，返回任务 ID，而不是让代理长时间保持响应连接。

## 验证与回滚 {#verification}

执行 `nginx -t` 后使用受控的 `reload`，对已知慢接口和正常接口各做一次测试，并确认 `error.log` 不再出现持续的上游超时。记录配置差异、观察窗口和回滚值；若错误率或延迟上升，恢复上一版本配置并继续排查上游，而不是继续叠加重试。

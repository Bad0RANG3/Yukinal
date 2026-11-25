+++
title = "Nginx 504：网关超时"
source = "https://nginx.org/en/docs/http/ngx_http_proxy_module.html#proxy_read_timeout"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_nginx01", "srv_nginx02"]
acl = ["public"]
+++

# Nginx 504：网关超时

## 现象 {#symptoms}

客户端收到 `504 Gateway Timeout`。504 可能是 Nginx 自己等待上游超时后生成的，也可能是上游应用、网关或 CDN 返回的。先看响应头、访问日志和 Nginx `error.log`，确认状态码来源，再决定是否调整代理参数。

## 超时链路 {#timeout-chain}

Nginx 到上游的超时至少包括：

- `proxy_connect_timeout`：建立到上游连接的时间。
- `proxy_send_timeout`：两次向 upstream 写数据之间的最大间隔。
- `proxy_read_timeout`：两次从 upstream 读到数据之间的最大间隔。
- 上游自身的数据库、缓存、HTTP 客户端和线程池超时。
- 外层负载均衡或 CDN 的超时。

任何一层先到期限，都可能表现为 504。只调大 Nginx 超时会延长占用连接的时间，不能消除上游慢请求。

## 只读诊断 {#diagnosis}

在访问日志中记录 `$upstream_status`、`$upstream_response_time`、`$request_time` 和 `$upstream_addr`。如果 `$upstream_status` 是 504，是上游返回了 504；如果为空而错误日志显示 `upstream timed out`，通常是 Nginx 生成。对疑似接口用一个小流量测试观察各阶段耗时，并检查上游连接池、锁、慢查询和外部依赖。

## 调整超时与后端策略 {#tuning}

先修复持续慢的上游，再设置与业务 SLA 一致的超时。对长任务使用异步任务和轮询，不把用户连接挂在代理上；对幂等 GET 可以考虑有限重试，对 POST/PUT 不得无条件重试。若使用 `proxy_next_upstream`，限制重试次数和目标，并记录实际命中的上游。

```nginx
proxy_connect_timeout 3s;
proxy_send_timeout 30s;
proxy_read_timeout 60s;
proxy_next_upstream error timeout;
proxy_next_upstream_tries 2;
```

这些只是示例值，不能替代容量评估。对 WebSocket 或流式响应，应单独评估读超时和空闲连接策略。

## 验证 {#verification}

用已知长短请求分别验证，观察错误率、P95/P99 延迟、上游在线实例数和连接数。确认 504 下降不是通过无限超时掩盖故障。记录变更前后的配置和观察窗口，保留快速回滚的旧配置。

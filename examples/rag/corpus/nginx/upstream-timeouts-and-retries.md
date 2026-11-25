+++
title = "Nginx 上游超时与重试策略"
source = "https://nginx.org/en/docs/http/ngx_http_upstream_module.html#proxy_next_upstream"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_nginx01", "srv_nginx02"]
acl = ["public"]
+++

# Nginx 上游超时与重试策略

## 参数含义 {#timeouts}

Nginx 的代理超时不是单一值。`proxy_connect_timeout` 控制建立连接，`proxy_send_timeout` 控制向上游发送数据的间隔，`proxy_read_timeout` 控制读取上游响应的间隔。上游只要持续有字节返回，读超时会重新计时；因此“总耗时很长”不一定等于 `proxy_read_timeout` 太短。访问日志中的 `$upstream_connect_time`、`$upstream_response_time` 和 `$request_time` 可以帮助区分等待位置。

## 重试带来的风险 {#retry-risk}

`proxy_next_upstream` 的默认重试集合和非幂等请求限制必须明确。重试可能把一次 POST 变成多次扣款、多次创建或重复消息；只要请求已经到达上游且上游可能已提交，就不能假设重试是安全的。对必须重试的写请求，应用层应使用幂等键和结果查询，而不是依赖代理透明重试。

## 只读诊断 {#diagnosis}

```bash
nginx -T 2>/dev/null | grep -nE 'proxy_(connect|send|read)_timeout|proxy_next_upstream|upstream'
tail -n 200 /var/log/nginx/error.log
```

把一次请求的 `$upstream_addr`、`$upstream_status`、`$upstream_connect_time`、`$upstream_response_time` 与上游自身日志关联。确认慢的是连接、首字节还是响应体传输。检查上游健康检查是否探测了真实接口，而不是只探测 TCP 端口。

## 变更顺序与验证 {#change-order}

先修复上游性能或故障，再设置明确的超时预算，最后才启用受限重试。一次只改一类参数，记录变更前值、预期指标和回滚值。变更后用一个小流量、可识别的测试请求验证路由和状态码，再观察一段时间错误率、重试次数、重复写入和连接数。不要把“无 502/504”作为唯一成功标准；重复副作用和延迟恶化同样是失败。

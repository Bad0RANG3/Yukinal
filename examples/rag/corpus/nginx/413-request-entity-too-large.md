+++
title = "Nginx 413：请求体过大"
source = "https://nginx.org/en/docs/http/ngx_http_core_module.html#client_max_body_size"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_nginx01", "srv_nginx02"]
acl = ["public"]
+++

# Nginx 413：请求体过大

## 现象 {#symptoms}

上传或提交表单时收到 `413 Request Entity Too Large`，Nginx 错误日志可能显示 `client intended to send too large body`。413 不一定由 Nginx 产生；上游应用、API 网关或对象存储也可能返回自己的 413。先通过响应头、访问日志和请求路径确认是哪一层拒绝。

## 限制可能来自哪一层 {#limit-layers}

- Nginx 的 `client_max_body_size`，默认值通常为 1 MB，可在 `http`、`server` 或 `location` 覆盖。
- 上游框架的 body 大小、multipart 文件数、字段数或超时限制。
- CDN、WAF 或负载均衡对请求体的独立上限。
- 客户端实际发送的是压缩后还是压缩前内容，以及是否使用了 chunked 传输。
- `client_body_buffer_size` 只影响内存缓冲，不会改变允许的最大请求体。

## 定位实际限制 {#diagnosis}

记录请求的 Host、URI、Content-Length、客户端和响应头，确认 413 是否来自 Nginx。检查所有可能生效的配置层级：

```bash
nginx -T 2>/dev/null | grep -nE 'client_max_body_size|client_body_buffer_size|location|server_name'
```

使用受控的小文件逐步增加体积，找出稳定触发点；不要在生产环境用超大请求压测。若反向代理前面还有网关，分别查看两层日志中的请求 ID。对于 `proxy_request_buffering off` 的场景，限制仍然生效，且磁盘临时文件策略也需要检查。

## 调整与验证 {#remediation}

只对确实需要上传的 `location` 设置合理的 `client_max_body_size`，并同步调整上游和前置网关限制。考虑分片上传、直传对象存储或异步处理，避免把大文件完整缓存在代理上。变更前检查磁盘、内存和超时预算；变更后执行 `nginx -t`，用边界大小文件验证成功和失败路径，确认状态码、日志和上游请求一致。

不要为了消除 413 直接设置无限大；请求体上限是资源保护边界，应结合认证、限速、磁盘配额和内容校验一起实施。

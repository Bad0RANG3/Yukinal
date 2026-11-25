+++
title = "Nginx 502 上游超时：团队专用处置手册"
source = "internal://team-ops/nginx/private-timeout-runbook"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_nginx01", "srv_nginx02"]
acl = ["team-ops"]
+++

# Nginx 502 上游超时：团队专用处置手册

## 私有超时与重发判定 {#private-upstream-timeout}

这份手册只对 `team-ops` 可见，用于演练 ACL 隔离；它包含与公开排障问题高度重叠的措辞：Nginx 返回 502，`error.log` 出现 `upstream timed out`，需要同时查看 `proxy_read_timeout`、`upstream_response_time` 和 `$upstream_addr`。如果代理层对已经到达上游的请求做透明重试，非幂等写请求可能被重发多次，因此必须优先核对幂等键和业务结果，而不是只看状态码。

## 私有变更记录 {#private-change-log}

任何超时参数调整都应记录旧值、预期收益、观察窗口和回滚值。该记录属于团队内部流程示例，不包含真实主机、凭据或生产地址。

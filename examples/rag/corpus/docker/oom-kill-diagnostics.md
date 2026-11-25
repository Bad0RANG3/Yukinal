+++
title = "Docker OOM 与 cgroup 诊断"
source = "https://docs.docker.com/engine/containers/resource_constraints/"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_docker01", "srv_docker02"]
acl = ["public"]
+++

# Docker OOM 与 cgroup 诊断

## 触发条件 {#trigger}

Docker 容器的内存限制最终由 Linux cgroup 实施。超过限制时，内核可能先回收页缓存，再触发 cgroup OOM killer；若没有设置限制，容器可能消耗宿主机内存并触发全局 OOM。排查时既要看容器配置，也要看宿主机总内存、swap、其他容器和内核日志。

## cgroup 证据 {#cgroup-evidence}

先取得容器完整 ID，并确认宿主使用 cgroup v2 还是 v1。v2 通常可查看 `memory.current`、`memory.peak`、`memory.max`、`memory.events`；v1 对应 `memory.usage_in_bytes`、`memory.limit_in_bytes` 和 `memory.oom_control`。路径会随发行版和容器运行时变化，应先定位再读取，不要硬编码。

```bash
docker stats --no-stream <container>
docker inspect --format 'oom={{.State.OOMKilled}} limit={{.HostConfig.Memory}} swap={{.HostConfig.MemorySwap}} pids={{.HostConfig.PidsLimit}}' <container>
cat /sys/fs/cgroup/memory.events 2>/dev/null || true
journalctl -k --since "2 hours ago" | grep -Ei 'oom|killed process|out of memory'
```

容器 cgroup 的 `memory.events` 中 `oom` 或 `oom_kill` 增长，说明限制命中；宿主全局 OOM 日志则说明压力可能来自多个进程。`docker stats` 显示的是当前采样，不能替代事件时间线。

## 区分容器 OOM 与主机 OOM {#distinguish-kill}

- 容器限制很低、cgroup 事件增长、其他容器正常：优先检查该容器的堆、缓存、并发和泄漏。
- 宿主机可用内存耗尽、多个进程被杀、内核日志出现全局 OOM：需要容量规划或隔离高内存任务。
- 容器 `OOMKilled=false` 但仍有 137：继续检查外部 `SIGKILL`、编排器驱逐、运行时或人工操作。
- 容器频繁达到限制但没有 OOM：可能是内存回收导致延迟上升，不能只看是否被杀。

## 修复与验证 {#remediation}

先用堆转储、火焰图、请求并发和对象增长趋势定位泄漏，再决定调整限制还是修代码。只提高 `--memory` 可能把问题转移给宿主机；只降低限制可能增加 OOM 和延迟。对缓存型服务设置清晰的淘汰策略，对 Java/Node.js 等运行时同时校准堆上限和容器上限，并检查 `PidsLimit`、文件描述符和线程数。

修复后使用接近生产的受控负载运行足够长时间，观察 `memory.current`、峰值、OOM 事件、重启次数和 P99 延迟。确认内存能在负载结束后回落，而不是缓慢单调增长；若仍增长，继续定位分配来源。

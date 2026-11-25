+++
title = "Docker 容器退出码 137"
source = "https://docs.docker.com/engine/containers/run/"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_docker01", "srv_docker02"]
acl = ["public"]
+++

# Docker 容器退出码 137

## 先确认事实 {#symptoms}

容器状态显示 `Exited (137)`，服务可能刚启动就消失，也可能运行一段时间后才退出。137 是 shell 约定的 `128 + 9`，表示进程收到 `SIGKILL`；它本身不能证明是内存不足。先保存容器 ID、启动时间、退出时间、重启次数和当时的宿主机状态，再判断杀进程的是 Docker、cgroup、内核、编排器还是人工操作。

## 137 的含义 {#meaning}

```bash
docker inspect --format 'id={{.Id}} status={{.State.Status}} exit={{.State.ExitCode}} oom={{.State.OOMKilled}} restarts={{.RestartCount}} started={{.State.StartedAt}} finished={{.State.FinishedAt}}' <container>
```

`OOMKilled=true` 是强证据，表示进程被内存控制组杀死；`OOMKilled=false` 也不能完全排除主机级内存压力或外部 `kill -9`。退出码 137 还常见于编排器驱逐、部署滚动更新、主机重启、运行时异常和人工 `docker kill`。查看 Docker events 和内核日志的时间线，时间必须与容器退出时间对齐。

```bash
docker events --since 60m --until now --filter container=<container>
journalctl -k --since "60 minutes ago" | grep -Ei 'out of memory|oom|killed process'
```

## 区分 OOM、主机杀进程和外部 SIGKILL {#triage}

- 容器有明确内存限制，`OOMKilled=true`，cgroup 的 `memory.events` 中 `oom_kill` 增长：优先按容器 OOM 处理。
- 容器没有内存限制，主机内存紧张，内核日志出现全局 OOM：问题在宿主机容量或其他进程，不能只调容器参数。
- 编排器执行了驱逐、缩容或滚动更新：检查控制面事件和部署记录。
- 进程收到应用自身的 `SIGKILL` 或容器运行时故障：结合 Docker daemon 日志和进程退出前的日志。
- 人工执行了 `docker kill`：检查操作审计，不要把人为操作误判为应用崩溃。

## 处置与验证 {#verification}

先避免无休止重启掩盖现场：暂停自动重启或迁移流量必须先评估可用性并保留操作记录。若是 OOM，按 `docker/oom-kill-diagnostics.md` 收集 cgroup 证据，修复内存泄漏或调整合理上限；若没有 OOM 证据，继续检查部署、宿主和编排事件。修复后用相同负载做回归，确认退出码、重启次数和内存曲线稳定，再恢复自动重启策略。

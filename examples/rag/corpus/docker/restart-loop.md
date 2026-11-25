+++
title = "Docker 容器重启循环"
source = "https://docs.docker.com/engine/containers/start-containers-automatically/"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_docker01", "srv_docker02"]
acl = ["public"]
+++

# Docker 容器重启循环

## 识别循环 {#loop-diagnosis}

容器反复从 `starting` 变成 `exited`，`RestartCount` 持续上升，日志被新一次启动覆盖，服务始终不可用。先记录容器 ID、镜像摘要、启动时间、退出码、重启策略和重启次数，并检查编排器是否也在重建容器。只在当前容器上反复执行 `restart` 会扩大故障，不会提供新的根因证据。

```bash
docker ps -a --no-trunc
docker inspect --format 'name={{.Name}} status={{.State.Status}} exit={{.State.ExitCode}} restarts={{.RestartCount}} policy={{.HostConfig.RestartPolicy.Name}} health={{json .State.Health}}' <container>
docker events --since 30m --filter container=<container>
```

## 读取上一次退出 {#log-inspection}

查看带时间戳的日志和上一次容器实例的日志，确认失败发生在配置加载、依赖连接、迁移、健康检查还是请求处理阶段：

```bash
docker logs --timestamps --tail 300 <container>
docker logs --previous --timestamps --tail 300 <container>
journalctl -u docker --since "30 minutes ago"
```

如果日志中只有启动横幅，检查进程是否在前台运行、入口脚本是否退出、配置文件权限是否可读，以及容器内路径是否被错误的 volume 覆盖。退出码 137 参见内存与 OOM 文档；退出码 1、2、126、127 往往分别指向应用错误、参数/配置错误、不可执行和命令不存在，但仍需结合镜像内容确认。

## 常见原因 {#common-causes}

- 必需的环境变量、密钥或配置文件缺失，进程启动后立即退出。
- 依赖的数据库、消息队列或网络端点不可达，健康检查失败后编排器重建。
- 数据迁移失败、卷权限不匹配或旧进程锁文件残留。
- 端口冲突、只监听容器内回环地址，或探针使用了错误路径。
- 镜像架构不匹配、入口命令被覆盖、运行时依赖缺失。
- 资源限制过小，导致启动阶段被 OOM 或 pids 限制终止。

## 停止循环并修复 {#remediation}

若持续重启影响其他服务，可在明确记录后暂时把重启策略改为不自动重启，或由编排器暂停滚动；这是可用性操作，必须评估影响并保留恢复步骤。修复配置或依赖后，用单实例前台日志验证启动流程，再恢复重启策略和副本数。验证至少包括：连续多次重启不再退出、健康检查通过、依赖短暂不可用时行为可预期、日志能保留上一次失败证据。

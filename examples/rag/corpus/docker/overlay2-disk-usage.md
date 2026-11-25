+++
title = "Docker overlay2 磁盘占用排查"
source = "https://docs.docker.com/engine/storage/drivers/overlayfs-driver/"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_docker01", "srv_docker02"]
acl = ["public"]
+++

# Docker overlay2 磁盘占用排查

## 先做空间盘点 {#space-accounting}

宿主机磁盘告警时，不要直接把 `/var/lib/docker/overlay2` 全部删除。先区分镜像层、容器可写层、卷、构建缓存、拉取缓存和容器日志，并检查文件系统、inode、挂载点以及 Docker root dir 的实际位置。

```bash
df -hT
docker info --format 'root={{.DockerRootDir}} driver={{.Driver}}'
docker system df -v
du -xhd1 /var/lib/docker 2>/dev/null | sort -h
```

`docker system df -v` 能分别列出镜像、容器、卷和构建缓存的空间。`du` 看到的是文件系统实际块；正在写入的日志或已删除但打开的文件可能让 `df` 与 `du` 不一致，后者应结合 `lsof +L1` 排查。

## overlay2、卷和缓存的区别 {#layers}

- 镜像层通常由多个容器共享，删除一个容器不会删除其基础镜像。
- 容器的可写层记录运行时修改，频繁写临时文件会让该层增长。
- 命名卷和绑定挂载位于 Docker 管理层之外，`docker system prune` 是否会清理取决于选项。
- 构建缓存来自 BuildKit 或旧构建器，常被误认为是运行中镜像。
- 容器日志默认由日志驱动写入，不属于镜像层，但在 `docker system df` 中可能被计入容器开销。

查看具体容器可写层时，结合 `docker inspect` 的 `GraphDriver.Data.UpperDir` 和 `SizeRw`，不要根据容器名猜测目录。

## 安全清理顺序 {#safe-remediation}

先清理可重建的构建缓存和明确未使用的 dangling 镜像，再评估停止的容器和未挂载卷。卷可能包含数据库或用户数据，删除前必须确认备份、引用关系和恢复方案。`docker system prune -a --volumes` 会删除大量对象，不是通用清理命令，禁止在未审查影响面的生产主机上直接执行。

```bash
docker image prune
docker builder prune
# 以下命令必须逐项核对对象，确认不包含持久数据后再执行
# docker container prune
# docker volume prune
```

同时处理持续增长的日志、临时上传文件和容器内缓存。对于容器内写入，应把需要持久化的数据放到卷或外部存储，并设置日志轮转和容量告警。

## 验证 {#verification}

清理前后记录 `df -hT`、`df -i`、`docker system df -v` 和关键容器状态。确认服务仍能读取持久数据、镜像可以重新拉取或构建、构建缓存删除后流水线时间符合预期。若空间很快再次耗尽，应定位写入来源，而不是周期性执行高风险 prune。

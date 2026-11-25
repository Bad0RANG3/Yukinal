+++
title = "Docker 容器日志增长与轮转"
source = "https://docs.docker.com/engine/logging/configure/"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_docker01", "srv_docker02"]
acl = ["public"]
+++

# Docker 容器日志增长与轮转

## 确认日志驱动 {#log-driver}

先确认 Docker daemon 和单个容器实际使用的日志驱动、路径与选项。默认 `json-file` 在没有配置轮转时会持续增长，但不同环境可能使用 `local`、`journald` 或远程驱动，不能套用同一清理办法。

```bash
docker info --format 'driver={{.LoggingDriver}}'
docker inspect --format 'driver={{.HostConfig.LogConfig.Type}} opts={{json .HostConfig.LogConfig.Config}}' <container>
```

## 定位增长来源 {#growth-source}

结合 `docker logs --tail` 观察是否出现请求循环、连接重试、堆栈刷屏或敏感字段；用宿主机的文件系统工具确认日志文件大小，但不要把日志内容写入工单或聊天记录。检查应用是否在每次请求都打印完整 payload，是否把调试级别带入生产，以及是否有错误循环导致短时间产生巨量日志。

日志增长可能来自容器 stdout/stderr，也可能来自容器内自行写入的 `/var/log` 文件。前者受 Docker 日志驱动管理，后者需要应用自己的 logrotate 或外部日志采集策略。两者要分别检查，不能只清理其中一个。

## 配置轮转 {#rotation}

对使用 `json-file` 的新容器，可以通过 daemon 配置设置默认上限：

```json
{
  "log-driver": "json-file",
  "log-opts": {
    "max-size": "100m",
    "max-file": "5"
  }
}
```

也可以在创建容器时用 `--log-opt max-size=100m --log-opt max-file=5` 覆盖。daemon 级配置通常只对新创建的容器生效；已有容器需要重建或由编排系统滚动替换，不能简单假设一次 daemon reload 就改变了旧容器的日志选项。修改 daemon 配置前检查 Docker 版本、运行方式和回滚路径。

## 验证与限制 {#verification}

创建测试容器并确认 `docker inspect` 中出现预期选项，持续写入后在文件大小达到阈值时观察轮转文件数量。生产变更要确认日志采集器仍能收集轮转后的文件，告警没有因日志延迟而失效。不要直接删除活跃日志文件或对未知文件执行截断；这可能丢失证据或破坏应用写入。轮转解决的是容量边界，不能替代修复日志刷屏和敏感信息输出。

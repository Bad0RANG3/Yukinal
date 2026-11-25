+++
title = "Linux inode 耗尽"
source = "https://man7.org/linux/man-pages/man1/df.1.html"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_linux01", "srv_linux02"]
acl = ["public"]
+++

# Linux inode 耗尽

## 现象 {#symptoms}

文件系统仍有可用字节，但创建文件、临时文件或锁文件时报 `No space left on device`。这类故障常见于小文件大量堆积：会话文件、邮件队列、缓存、容器层、临时目录、未轮转的小日志和异常重试产生的碎片文件。先用 `df -ih` 确认 inode 使用率，再定位目录。

## inode 盘点 {#inode-accounting}

```bash
df -ih
du --inodes -x -d1 /var 2>/dev/null | sort -n
find /var -xdev -type f -printf '.' 2>/dev/null | wc -c
```

`du --inodes` 依赖 GNU coreutils；没有该选项时可用受控的 `find` 计数。不同文件系统分配 inode 的策略不同，XFS 也并非无限 inode，不能因为底层是 XFS 就忽略耗尽。目录本身、硬链接和特殊文件也会占用元数据；硬链接不会为同一 inode 重复计数。

## 常见耗尽来源 {#common-causes}

- 应用为每个请求、会话或重试创建小文件，但没有清理。
- 邮件队列、任务队列或上传暂存区积压。
- 容器 overlay 或临时层产生大量小文件。
- 缓存目录保留了过多版本或过期对象。
- 监控、调试或审计日志按分钟切分且长期不删除。
- 备份工具生成了大量增量碎片，未按策略合并。

## 清理与验证 {#remediation}

按目录计数和修改时间定位来源，先修复持续创建文件的根因，再处理可删除对象。不要用宽泛的 `find / -delete`；删除范围必须限制在确认过的目录、文件类型和保留窗口内。清理后再次检查 `df -ih`、目录计数和应用创建临时文件的能力。若目录数量本身是业务数据，应调整分片或归档策略，而不是反复清理。为 inode 设置独立告警；只监控字节容量会漏掉这类故障。

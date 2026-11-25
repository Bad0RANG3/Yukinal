+++
title = "Linux 磁盘空间耗尽"
source = "https://man7.org/linux/man-pages/man1/df.1.html"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_linux01", "srv_linux02"]
acl = ["public"]
+++

# Linux 磁盘空间耗尽

## 现象与先决条件 {#symptoms}

应用报 `No space left on device`，写入失败、日志中断，某些进程可能仍能读写已打开文件。先确认是哪一个文件系统、哪一个挂载点，以及告警来自容量、inode 还是配额。容器和只读根文件系统会让宿主机与容器内的 `df` 结果不同，必须在实际写入的命名空间和挂载点检查。

```bash
df -hT
df -ih
findmnt -T <path>
```

## 区分空间与 inode {#space-accounting}

`df -h` 的 `Use%` 接近 100% 表示块空间不足；`df -i` 的 `IUse%` 接近 100% 则可能是小文件耗尽 inode，即使还有大量字节。两者要同时检查。已删除但仍被进程打开的文件会占用块空间，因此 `df` 可能很高而 `du` 找不到对应文件；继续按 `deleted-open-files.md` 检查。

## 找出增长点 {#growth-diagnosis}

从受影响的挂载点开始，限制扫描范围并保留错误输出：

```bash
du -xhd1 /var 2>/dev/null | sort -h
find /var -xdev -type f -size +500M -printf '%s %p\n' 2>/dev/null | sort -n | tail -20
journalctl --disk-usage 2>/dev/null || true
```

依次检查应用日志、journald、Docker/containerd 数据、包缓存、临时上传、备份和数据库 WAL。不要直接扫描整个根目录或跨挂载点递归，避免拖慢生产主机。对日志增长检查是否刷屏，对数据库增长检查 WAL、复制槽和备份，对缓存检查保留策略。

## 安全清理 {#safe-cleanup}

先停止继续写入的上游原因，再选择可重建对象清理。包缓存、过期构建缓存和旧日志通常可以按保留策略处理；数据库、卷、备份和用户上传不能仅凭“看起来没用”删除。日志轮转和 journald vacuum 比手工删除活跃文件更安全。涉及服务重启、删除快照或扩容时，必须评估影响面并保留回滚。

## 验证与防止复发 {#verification}

清理后比较 `df -hT` 和 `df -i`，确认应用能写入、日志能采集、关键服务健康。观察一个完整业务周期，确认空间没有立即反弹。为不同挂载点设置容量和 inode 告警，给日志、缓存和备份设置上限；如果根因是数据增长，应扩容或迁移数据，而不是长期依赖人工清理。

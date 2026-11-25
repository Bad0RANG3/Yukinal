+++
title = "Linux 已删除但仍打开的文件"
source = "https://man7.org/linux/man-pages/man8/lsof.8.html"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_linux01", "srv_linux02"]
acl = ["public"]
+++

# Linux 已删除但仍打开的文件

## 现象 {#symptoms}

`df` 显示文件系统接近满，但 `du` 找不到等量大文件，清理已知目录后空间没有释放。进程可能仍持有已 unlink 的文件描述符，尤其是日志被手工删除、日志轮转失败或应用仍向旧文件写入。确认受影响挂载点、服务进程和时间线，再寻找 `(deleted)` 句柄。

## 为什么 df 和 du 不一致 {#df-du-gap}

`df` 统计文件系统已分配的块；`du` 遍历目录项和链接。文件被删除后，若仍有进程打开它，inode 和数据块不会释放，但目录中已经没有路径。`df` 与 `du` 的差异也可能来自保留块、稀疏文件、快照、容器命名空间和权限导致 `du` 看不全，不能只凭一次 `du` 判定。

## 找出已删除但仍打开的文件 {#open-deleted}

```bash
lsof +L1 2>/dev/null
lsof -nP 2>/dev/null | grep '(deleted)'
ls -l /proc/<pid>/fd 2>/dev/null | grep deleted
```

确认 PID、可执行文件、文件描述符、大小、挂载点和所属服务。容器中的句柄可能不在宿主机普通路径下，需要结合容器 PID 命名空间和 cgroup 检查。不要杀掉不认识的进程来“释放空间”；先确认服务角色和可接受的中断窗口。

## 安全恢复 {#safe-recovery}

优先让应用安全重开日志或重启持句柄的服务。若文件是纯日志且应用明确支持截断，可以对对应 `/proc/<pid>/fd/<n>` 做受控截断，但必须确认不是数据库、备份、上传或正在写的事务文件；对这类文件截断会造成损坏。不要用 `rm` 继续删除路径，因为路径可能已经不存在。保留操作前后的 `df`、`lsof` 和服务日志，验证空间释放且应用没有报错。

+++
title = "systemd daemon-reload、drop-in 与覆盖"
source = "https://www.freedesktop.org/software/systemd/man/latest/systemctl.html#daemon-reload"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_app01", "srv_app02"]
acl = ["public"]
+++

# systemd daemon-reload、drop-in 与覆盖

## 何时需要重载 {#when-to-reload}

修改或新增 `.service`、`.timer`、`.socket` 或 drop-in 文件后，systemd 管理器的单元缓存不会自动更新，需要执行：

```bash
systemctl daemon-reload
```

`daemon-reload` 只重新读取单元定义，不会自动重启正在运行的服务，也不会让已启动进程获得新的 `ExecStart`、环境变量或资源限制。完成重载后要按变更类型重启服务；某些配置在服务运行期间不会重新加载，必须显式 stop/start。

## reload 与 reexec 的区别 {#reload-vs-reexec}

`systemctl daemon-reload` 重读配置；`systemctl daemon-reexec` 会让 systemd 管理器进程重新执行自身，属于更高风险的时间点操作，通常不是修复单元配置的首选。不要用 reexec 代替重读单元，也不要在没有维护窗口和回滚计划时执行它。

## drop-in 的覆盖顺序 {#override-order}

推荐的持久化覆盖方式是 `systemctl edit <unit>` 生成的 `/etc/systemd/system/<unit>.d/*.conf`。多个 drop-in 按文件名排序读取；后读取的值可能覆盖前面的同名键。多个目录之间也有优先级，必须用以下命令确认最终结果，而不是只看某个文件：

```bash
systemctl cat <unit>
systemctl show <unit> -p FragmentPath,DropInPaths,ExecStart,Environment,Restart
```

当值看起来“改了无效”时，检查是否有更高优先级 drop-in、mask 单元、同名的实例单元，或服务自身读取了单独的配置文件。文件权限也不能过宽，避免非特权用户修改启动命令或环境文件。

## 验证 {#verification}

执行 `daemon-reload` 后确认 `systemctl show` 中的最终值和 `systemctl cat` 的加载顺序符合预期，再重启测试实例并检查退出码、日志和环境。记录单元文件路径、drop-in 路径、变更前后的值和回滚方式。若服务仍运行旧配置，先检查是否没有重启或应用自行缓存了配置，而不是继续修改 systemd 单元。

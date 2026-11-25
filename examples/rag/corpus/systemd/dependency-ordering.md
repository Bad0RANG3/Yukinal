+++
title = "systemd 依赖与启动顺序"
source = "https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_app01", "srv_app02"]
acl = ["public"]
+++

# systemd 依赖与启动顺序

## 启动顺序不等于依赖 {#ordering-vs-requirement}

`After=` 只规定启动先后，不会自动把被引用单元拉起来；`Requires=` 会建立强依赖，但通常还需要配合 `After=` 才能表达“先启动后运行”。`Wants=` 是较弱的依赖，目标启动失败不一定阻止当前单元；`BindsTo=` 更严格，目标停止时可能影响当前单元。理解这些差异可以避免“服务明明依赖数据库，却仍先启动并崩溃”。

## 查看依赖图 {#dependency-graph}

```bash
systemctl list-dependencies --all <unit>
systemctl show <unit> -p Requires,Wants,After,Before,BindsTo,PartOf,RequiresMountsFor
systemd-analyze critical-chain <unit>
journalctl -b -u <unit> --no-pager -n 200
```

注意 `network.target` 不保证外部网络和 DNS 已就绪；依赖远程存储时，通常需要专门的挂载单元、健康检查或应用重试。socket 激活要确认 socket 单元与 service 单元的对应关系和权限。

## 常见错误 {#common-errors}

- 只写 `Requires=` 没写 `After=`，导致依赖虽然被启动，当前服务仍可能先运行。
- 依赖缺失的挂载点、网络或数据库，但服务没有重试，直接把临时故障变成永久失败。
- 使用 `Wants=` 后又假设依赖必然成功。
- 把应用内部初始化全部塞进启动阶段，超时后反复重启。
- 忽略 `User`、`Group`、环境变量和运行时目录在依赖单元中的创建顺序。
- 依赖循环导致单元无法达到稳定状态。

## 验证 {#verification}

在测试环境分别验证依赖正常、依赖启动较慢、依赖暂时不可用和依赖退出这四种情况。观察启动顺序、退出码、重启行为和健康检查。修复后更新依赖图说明，并确认 `daemon-reload`、重启和重新启动依赖仍能得到相同结果。不要用增大 `TimeoutStartSec` 掩盖没有就绪信号的问题。

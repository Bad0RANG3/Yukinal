# 服务器检测工具包

连接成功后，客户端立即预取服务器快照；右侧面板复用同一请求和 15 秒缓存。快照仍通过一次 SSH 批处理采集，UI 与 `server.info` 使用同一份解析结果结构，不安装常驻服务。

默认模块：系统、CPU、内存、磁盘、运行时间、网络、Docker、GPU、安全信息。GPU 当前支持 NVIDIA `nvidia-smi`，包括利用率、显存与温度；工具缺失、超时、N/A 和无设备分别保留状态，不当成 0%。CPU 使用前八个计数器，排除 guest 重复计数，将 iowait 计入空闲；计数器未推进时显示采集失败。

安全信息只读取当前 UID、`sshd -T` 默认配置、UFW/firewalld 状态以及 TCP/UDP 监听地址。权限不足为“未确认”。SSH Match 条件、云防火墙、其他防火墙实现、公网可达性与漏洞扫描不在这些检测结果中，也不依据本结果宣称服务器安全。

## 扩展模块

1. 在 `agent/src/yukinal_ssh/toolkit_catalog.json` 加入固定只读探针：唯一 `id`、模块 `module`、`required: false`、`timeoutSeconds` 和 `command`。此文件随程序打包；不接受远端或对话提供的可执行插件。新增探针保持可选，以兼容旧服务。
2. 在 `agent/src/yukinal_agent/server_toolkit.py` 注册模块解析函数。用独立字段返回结果；单项无法确认不得覆盖其他项。缺失字段代表未报告，成功空列表才代表没有条目。同步添加共享 TypeScript 类型与严格 schema。
3. 在右侧面板添加精简摘要和可展开详情。新增采集状态纳入诊断目录；未知状态不显示为正常。
4. 添加解析、旧版兼容、超时/权限失败与 schema 测试。编译原生组件后运行 `scripts/test-snapshot-toolkit.py --native-tests <yukinal_ssh_tests> --shell <POSIX shell>`，验证实际生成的命令。Windows 可使用 Git Bash。

CMake 将同一目录编译进 C++，Python 从包资源读取；PyInstaller 收集目录文件。修改目录会触发 CMake 重新配置。

## 执行约束

- 可能阻塞的探针限时 2 秒，另留 1 秒强制终止时间；缺少 `timeout` 时跳过此类探针，不安装软件或请求提权。
- 单探针输出最多 16 KiB，总数据预算 48 KiB，为 SSH 的 64 KiB 输出限制预留分隔标记空间。超限整项失败，不解析截断结果。
- Docker `info` 只执行一次；失败后跳过容器列表。整体 SSH 请求仍受主机 25 秒超时约束。
- 断开后停止轮询并保留原始时间戳；重新连接强制采集；不同服务器使用独立缓存并核验返回的服务器 ID。

本地命令夹具验证的是协议、隔离、限时和显示契约，不代替真实服务器兼容性验收。

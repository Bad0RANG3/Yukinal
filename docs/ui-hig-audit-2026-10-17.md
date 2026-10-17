# Yukinal 界面检查与修复记录

日期：2026-10-17。对象：当前打开的新版，`codex/ui-layout-direction` 工作区。检查参考 Apple Human Interface Guidelines，保留现有 Windows 桌面操作习惯、深色视觉、绿色 Y、固定侧栏宽度、四列看板与 16:9 卡片。

## 结论与检查边界

已完成当前导航中各页面的界面巡检、共用控件检查和已确认问题的修复。最新两项反馈已在重新编译的 Windows 原生窗口验证：设置下拉菜单及其展开列表使用统一深色样式；最大化时 AI 区域与标题栏顶部连续，还原窗口时恢复原有留白和圆角。

修复后的源码清单覆盖 65 个 TSX 文件，记录 621 个交互或语义元素，其中包含 258 个按钮和 97 个输入、选择、文本区域。这里的元素数包括标签、状态区域及条件分支，并非 621 个独立控件均被逐一点击。旧仪表台、排查任务和项目页残留源码共 81 个元素，已单列为停用入口，不计作当前页面实测。

验证分为三类：**实测**表示已打开页面并操作所述状态；**源码／检查**表示追踪组件和现有自动检查；**条件待测**表示需要真实连接、授权或数据变化，本轮未执行该动作。所有组件均纳入清单，条件待测状态保留边界，不能据此宣称全量辅助技术或 WCAG 认证。

## 本轮修复

| 问题 | 修复后的行为 | 验证 |
| --- | --- | --- |
| 设置下拉列表沿用系统外观，长名称挤掉箭头、展开越界 | 原生单选控件统一深色菜单、悬停、选中、禁用与焦点状态；收起时限制长文字，展开按控件宽度换行并限制高度；旧内核保留深色回退 | 原生字体下拉三项、浏览器超长模型连接名称截图 |
| 最大化后 AI 上缘与标题栏脱节 | 原生宿主返回最大化状态，前端与原生顶部同步去掉间距和上圆角；还原恢复原样；相邻网格的小数像素误差在布局边界处理 | 原生最大化／还原、边界回归检查；IPC 校验保持严格 |
| 小图标、资源展开、复选框标签可点击范围偏小 | 共用按钮与展开控件采用至少 24 px 桌面命中高度，复选／单选标签扩大点击范围，补齐键盘焦点 | 浏览器尺寸测量、添加服务器认证表单 |
| 模型和推理强度出现层叠菜单 | 合并到同一个弹层；方向键、Home、End、Enter、Space 可选择，Escape 关闭并返回触发按钮 | 长模型列表和推理强度实测 |
| 输入建议缺少明确的活动项关系 | 对话中的团队、服务器、命令建议补齐列表与活动项关联，退出动画期间不可交互 | @ 与 /、方向键、Escape 实测 |
| 角色、MCP、记忆编辑打开后焦点位置不明确 | 打开后定位首个编辑字段，保存或取消后返回对应入口 | 三种编辑器打开／取消实测；未保存数据 |
| OAuth 等待时 Escape 生命周期不完整，打开网页失败不可见 | 等待中 Escape 走取消流程，结束后走关闭；按钮焦点、复制结果与打开失败可被感知 | 源码与回归检查；真实授权流程条件待测 |
| 传输总量未知时显示虚假的 0% 或 100% | 未知总量使用不定进度；明确完成才显示 100%，正常数值限制范围，标注传输方向／文件／状态 | 三项进度逻辑检查；真实传输条件待测 |
| 终端首次连接失败缺少可见恢复路径 | 连接打开失败进入错误反馈，可使用现有重连按钮恢复 | 源码及构建；真实 SSH 失败注入条件待测 |
| 主机信任说明与实际校验流程不一致 | 首次验证和固定主机密钥的说明一致；校验读取失败增加重试入口 | 原生策略调用链与回归检查；真实信任操作未执行 |
| 服务器读取错误缺少明确状态语义 | 读取错误作为警告区域呈现 | 源码／检查 |
| 从工具入口打开工作页后焦点仍留在旧入口 | 用户打开后焦点进入对应工作页标签；后台自动打开不抢用户焦点 | 文件工作页活动标签实测 |

24 px 是本项目面向鼠标和键盘的紧凑桌面基线，不是把 Apple 的触控尺寸要求改写成桌面通用规范。文字、焦点、间距及动效同时参考 HIG 的可读性、可感知性和渐进披露原则。

## 页面与状态覆盖

| 页面／控件族 | 已实测的状态 | 条件待测或仅源码检查 |
| --- | --- | --- |
| 窗口、导航、AI 主区 | 原生最大化、还原、宽／紧凑布局；设置与参考面板进入／返回；顶部衔接；未改变面板宽度 | 多显示器、不同 DPI、独立工作页窗口完整组合 |
| 动态 | 全部／异常筛选、按日期分组、失败条目展开、长内容、工具记录 | 大规模持续事件流和真实异常恢复 |
| 服务器侧栏 | 搜索、资源展开、Docker 子项、更多菜单、两页属性、Escape 返回 | 真实连接、断开、主机信任写入、删除 |
| 添加服务器 | 非模拟入口、首次焦点、认证类型切换、SSH 证书字段、取消和焦点归还 | 保存、密钥选择及真实认证挑战；未填写凭据 |
| 看板 | 四列、16:9 卡片、长任务标题、任务属性三页、并行操作的卡片／导航／服务器描边 | 真实并行任务、审批与取消；紧凑宽度需横向滚动 |
| 团队 | 团队列表、配置、角色编辑／取消、值守计划、长目标、错误条目 | 实际团队执行、定时循环以及跨会话长时间运行 |
| 记忆库 | 查看、新增编辑器、首字段焦点、取消、返回入口 | 保存／删除、切换范围时的草稿保留策略 |
| 对话 | 新任务、已有最近对话、历史搜索和筛选；@／命令建议；单层模型选择与推理强度；添加菜单 | 实际发送与流式输出、权限审批、历史重命名／删除依赖源码和已有检查 |
| 设置：偏好 | 字体、字号、行高和批准方式下拉外观；禁用／标签／焦点、长值 | 未更改批准策略、未重置用户偏好 |
| 设置：模型 | 五分区切换、连接选择、长名称、模型高级区及表单布局 | 凭据保存、外部模型探测和删除 |
| 设置：工具 | MCP 列表、工具列表、编辑器、首字段焦点与取消 | 真正设备码授权、取消网络请求、工具执行 |
| 设置：网络／诊断 | 网络表单、诊断状态和日志区域 | 网络设置保存、代理故障与外部探测 |
| 服务器概览 | 指标、状态展开、网络区域、服务列表、日志内容 | 真实指标刷新和断线恢复 |
| 文件 | 五条模拟条目、筛选、排序、文本预览、本地抽屉、工具页焦点 | 上传／下载／冲突／取消／校验只做源码和进度检查，未读写服务器文件 |
| 终端 | 实际 xterm 渲染与工作页布局；静默模拟连接 | 真实 SSH 输入、关闭、重连、复制粘贴与长输出 |
| 操作记录和证据 | 三条模拟记录、事件详情、服务证据工作页、输入／结果／状态／策略信息 | 真实操作证据、失败链路与保留期 |
| 二级属性窗口 | 服务器属性、任务属性、标签切换、Escape 与焦点返回 | 系统认证／权限窗口不在自动化操作范围 |
| 引导 | 首步布局、入口和标签 | 后续依赖成功模型／服务器配置的步骤以源码检查为主 |

## 共用规则检查

- **布局与层级**：主导航、服务器树、主对话、工作页、参考区和属性窗口分别承载稳定职责；主要状态优先于说明文字。未新增套层圆角框。
- **文字与长内容**：检查服务器长名、任务长标题、长模型列表及错误展开；选择列表可换行，收起控件保持箭头可见。基础文字色与焦点色做了深色背景对照，未把个别 token 测量推断为全部内容合规。
- **键盘和焦点**：按钮命名、选项卡、组合框、编辑器首焦点、返回入口、Escape、建议列表活动项与工作页焦点均纳入检查。
- **状态反馈**：错误使用警告语义；复制使用状态反馈；未知传输进度不伪造百分比；执行状态保留文字／团队名和描边反馈。
- **滚动**：保留普通滚轮与键盘滚动方式；聊天滚动轨道、列表、菜单和日志滚动容器追踪到源码。未拦截系统滚轮偏好。
- **动效与适应性**：检查已有减少动态效果和强制颜色规则；本轮未切换系统偏好，也未运行完整屏幕阅读器流程。
- **启发式误报复核**：清单中的 11 个引导按钮未位于表单内，没有意外提交路径；文本输入区的点击处理用于正常输入和建议关闭，不是唯一操作入口。

## 验证结果与证据

- 前端类型检查通过，369 项自动检查全部通过，无失败或跳过。
- 最新前端发布构建通过；Windows 原生宿主重新编译通过。
- 原生 `window_context_tests`、`caption_hit_test_tests`、`desktop_host_self_check` 三项通过。
- 原生真实窗口验证最大化、还原、下拉列表和紧凑布局；浏览器 1280 × 720 记录了 28 组页面／状态测量。
- `git diff --check` 通过；现有未提交工作保留。

证据目录：[UI 检查输出](./verification/ui-hig-20261017)。其中原生截图可能包含用户已有的性能监控覆盖层，它不属于 Yukinal 页面。

| 证据 | 内容 |
| --- | --- |
| [最新最大化设置截图](./verification/ui-hig-20261017/native-settings-maximized.png) | AI 贴顶与深色下拉菜单 |
| [还原窗口](./verification/ui-hig-20261017/native-restored.png) | 还原后的间距与圆角 |
| [长名称收起](./verification/ui-hig-20261017/browser-long-select-closed.png)／[展开](./verification/ui-hig-20261017/browser-long-select-open.png) | 箭头、菜单宽度与完整选项换行 |
| [原生看板](./verification/ui-hig-20261017/native-board.png) | 四列及并行操作提示 |
| [紧凑对话](./verification/ui-hig-20261017/native-compact-chat.png)／[紧凑设置](./verification/ui-hig-20261017/native-compact-settings.png) | 参考面板进入与返回 |
| [最终组件清单](./verification/ui-hig-20261017/control-inventory.csv) | 65 个 TSX 文件的元素位置、类型和语义属性表达式 |
| [页面测量](./verification/ui-hig-20261017/page-measurements.csv) | 28 组页面状态的控件数、视口和页面宽度；完整尺寸及颜色测量留存于本地验收输出 |
| [前端检查记录](./verification/ui-hig-20261017/frontend-tests.txt) | 369 项通过 |

## 尚存限制

1. 在约 1105 px 宽窗口同时呈现四列看板和临时对话时，需要横向滚动看板。当前保留用户指定的四列和面板宽度，不能声称紧凑窗口能同时完整显示四列。
2. 临时对话借用右侧时，与设置或团队配置叠加的组合仍需关注可用空间；现有“返回中间”与紧凑布局的“回到对话”保留退出路径。
3. 真实 SSH、MCP OAuth、权限批准、文件传输、删除和持续监控状态尚未做端到端实测。已检查可达分支和共用控件，后续需在专用测试环境验证。
4. 未覆盖不同 DPI、多显示器、旧版 WebView2、系统高对比和屏幕阅读器的完整组合；不将本次界面巡检表述为这些环境全部验收通过。

## Apple HIG 参考

采用可读性、明确反馈、稳定布局、直接操作、渐进披露、键盘可达和尊重动态效果偏好的原则。Windows 的窗口按钮、表单语义和系统行为保留本平台约定。

- [Accessibility](https://developer.apple.com/design/human-interface-guidelines/accessibility)
- [Layout](https://developer.apple.com/design/human-interface-guidelines/layout)
- [Typography](https://developer.apple.com/design/human-interface-guidelines/typography)
- [Buttons](https://developer.apple.com/design/human-interface-guidelines/buttons)
- [Popovers](https://developer.apple.com/design/human-interface-guidelines/popovers)
- [Menus](https://developer.apple.com/design/human-interface-guidelines/menus)
- [Keyboards](https://developer.apple.com/design/human-interface-guidelines/keyboards)
- [Alerts](https://developer.apple.com/design/human-interface-guidelines/alerts)
- [Sidebars](https://developer.apple.com/design/human-interface-guidelines/sidebars)
- [Toolbars](https://developer.apple.com/design/human-interface-guidelines/toolbars)
- [Text fields](https://developer.apple.com/design/human-interface-guidelines/text-fields)
- [Progress indicators](https://developer.apple.com/design/human-interface-guidelines/progress-indicators)
- [Modality](https://developer.apple.com/design/human-interface-guidelines/modality)
- [Feedback](https://developer.apple.com/design/human-interface-guidelines/feedback)
- [Scroll views](https://developer.apple.com/design/human-interface-guidelines/scroll-views)

## 组件清单覆盖索引

下表的数量是源码元素数。实测仅指前面明确列出的页面状态；不能将某个组件的代表状态实测外推到该组件所有条件分支。

| 组件（相对 apps/desktop/src） | 元素 | 覆盖方式 |
| --- | ---: | --- |
| `app/AppShell.tsx` | 11 | 代表状态实测＋源码；具体边界见页面表 |
| `app/LivePreviewControl.tsx` | 1 | 代表状态实测＋源码；具体边界见页面表 |
| `app/ServerInspectorHeader.tsx` | 2 | 代表状态实测＋源码；具体边界见页面表 |
| `app/WindowFrame.tsx` | 1 | 代表状态实测＋源码；具体边界见页面表 |
| `app/WorkpageWindow.tsx` | 3 | 源码；独立窗口条件待测 |
| `components/EnvBadge.tsx` | 0 | 源码；随所属页面检查表现 |
| `components/Icon.tsx` | 0 | 源码；随所属页面检查表现 |
| `components/KeywordText.tsx` | 0 | 源码；随所属页面检查表现 |
| `components/MarkdownText.tsx` | 2 | 源码；随所属页面检查表现 |
| `components/PageToolbar.tsx` | 0 | 源码；随所属页面检查表现 |
| `components/PanelStates.tsx` | 1 | 源码；随所属页面检查表现 |
| `components/SecondaryWindow.tsx` | 9 | 代表状态实测＋源码；具体边界见页面表 |
| `features/activity/ActivityFeed.tsx` | 7 | 代表状态实测＋源码；具体边界见页面表 |
| `features/agent/AgentComposer.tsx` | 23 | 代表状态实测＋源码；具体边界见页面表 |
| `features/agent/AgentEntryView.tsx` | 6 | 代表状态实测＋源码；具体边界见页面表 |
| `features/agent/AgentFeed.tsx` | 1 | 源码；真实流式内容条件待测 |
| `features/agent/AgentHeader.tsx` | 5 | 代表状态实测＋源码；具体边界见页面表 |
| `features/agent/AgentHistoryDeleteConfirm.tsx` | 2 | 源码／已有检查；未执行删除 |
| `features/agent/AgentHistoryGroupHeader.tsx` | 0 | 源码；随所属页面检查表现 |
| `features/agent/AgentHistoryPane.tsx` | 9 | 代表状态实测＋源码；具体边界见页面表 |
| `features/agent/AgentHistoryRenameForm.tsx` | 4 | 源码／已有检查；未提交重命名 |
| `features/agent/AgentHistoryRow.tsx` | 5 | 代表状态实测＋源码；具体边界见页面表 |
| `features/agent/AgentHistoryToolbar.tsx` | 5 | 代表状态实测＋源码；具体边界见页面表 |
| `features/agent/AgentNotices.tsx` | 6 | 源码；审批和运行错误条件待测 |
| `features/agent/AgentPanel.tsx` | 6 | 代表状态实测＋源码；具体边界见页面表 |
| `features/agent/AgentRecentConversations.tsx` | 4 | 代表状态实测＋源码；具体边界见页面表 |
| `features/agent/ConversationScrollRail.tsx` | 1 | 代表状态实测＋源码；具体边界见页面表 |
| `features/agent/CoordinatorTasks.tsx` | 6 | 源码；真实 Dots 执行条件待测 |
| `features/agent/MemoryLibrary.tsx` | 19 | 代表状态实测＋源码；具体边界见页面表 |
| `features/agent/ModelPicker.tsx` | 3 | 代表状态实测＋源码；具体边界见页面表 |
| `features/agent/PermissionPicker.tsx` | 5 | 菜单外观实测；未改变批准策略 |
| `features/agent/ReasoningPicker.tsx` | 4 | 代表状态实测＋源码；具体边界见页面表 |
| `features/agent-nist/AgentNistPane.tsx` | 28 | 代表状态实测＋源码；具体边界见页面表 |
| `features/agent-nist/TaskPropertiesWindow.tsx` | 4 | 代表状态实测＋源码；具体边界见页面表 |
| `features/agent-nist/TeamActivity.tsx` | 2 | 模拟并行反馈实测；真实任务条件待测 |
| `features/agent-nist/TeamBoardPane.tsx` | 10 | 代表状态实测＋源码；具体边界见页面表 |
| `features/agent-nist/TeamWatchPane.tsx` | 7 | 列表实测；循环执行条件待测 |
| `features/files/RemoteFilesPane.tsx` | 30 | 列表／预览实测；传输操作源码／条件待测 |
| `features/investigations/InvestigationsPane.tsx` | 70 | 停用入口；源码清单保留 |
| `features/logs/LogsPane.tsx` | 2 | 代表状态实测＋源码；具体边界见页面表 |
| `features/onboarding/GettingStarted.tsx` | 25 | 首步实测；后续步骤源码／条件待测 |
| `features/overview/InfrastructureDashboard.tsx` | 10 | 停用入口；源码清单保留 |
| `features/overview/ServerNetworkSection.tsx` | 3 | 模拟概览实测；真实数据条件待测 |
| `features/overview/ServerOverview.tsx` | 17 | 代表状态实测＋源码；具体边界见页面表 |
| `features/projects/ProjectsPane.tsx` | 1 | 停用入口；源码清单保留 |
| `features/servers/AddServerModal.tsx` | 52 | 代表状态实测＋源码；具体边界见页面表 |
| `features/servers/HostKeyGate.tsx` | 3 | 源码／检查；真实验证条件待测 |
| `features/servers/HostKeySection.tsx` | 7 | 源码／检查；信任修改条件待测 |
| `features/servers/ServerAuthChallengeModal.tsx` | 7 | 源码；认证挑战条件待测 |
| `features/servers/ServerList.tsx` | 16 | 代表状态实测＋源码；具体边界见页面表 |
| `features/servers/ServerPropertiesWindow.tsx` | 0 | 代表状态实测＋源码；具体边界见页面表 |
| `features/servers/ServerRowActions.tsx` | 2 | 代表状态实测＋源码；具体边界见页面表 |
| `features/servers/ServerTreeItem.tsx` | 9 | 代表状态实测＋源码；具体边界见页面表 |
| `features/services/ServicesPane.tsx` | 1 | 代表状态实测＋源码；具体边界见页面表 |
| `features/settings/McpOAuthDeviceCodeDialog.tsx` | 10 | 源码／回归检查；真实 OAuth 条件待测 |
| `features/settings/McpSettings.tsx` | 52 | 代表状态实测＋源码；具体边界见页面表 |
| `features/settings/NetworkSettings.tsx` | 14 | 页面实测；未保存网络配置 |
| `features/settings/PreferenceSettings.tsx` | 8 | 代表状态实测＋源码；具体边界见页面表 |
| `features/settings/ProviderPicker.tsx` | 6 | 代表状态实测＋源码；具体边界见页面表 |
| `features/settings/RuntimeSettings.tsx` | 35 | 代表状态实测＋源码；具体边界见页面表 |
| `features/terminal/TerminalPane.tsx` | 2 | 原生与浏览器布局实测；真实 SSH 条件待测 |
| `features/workpages/WorkpageArtifactView.tsx` | 4 | 代表状态实测＋源码；具体边界见页面表 |
| `features/workpages/WorkpagesDock.tsx` | 29 | 代表状态实测＋源码；具体边界见页面表 |
| `features/workpages/WorkspaceLayers.tsx` | 4 | 代表状态实测＋源码；具体边界见页面表 |
| `main.tsx` | 0 | 源码；随所属页面检查表现 |

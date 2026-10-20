# AI 对话可视化协议 v1

模型在正常 Markdown 回复中输出一个 `yukinal-visual` fenced JSON block，客户端本地绘制图形。绘图适合趋势、对比、函数坐标、流程关系和三维信息结构；简单问答继续使用普通文字。图形只表达该回复提供的数据，不代表已执行操作、已验证证据或后台实时采样。未采集的数据必须说明为示例，禁止捏造测量结果。

实现入口：`apps/desktop/src/lib/visual-document.ts` 的 `parseVisualDocument`；Markdown 入口为 `MarkdownVisualBlock`，四种绘图由 `ConversationVisual` 渲染。无需安装绘图依赖，无网络请求、iframe、HTML 或脚本执行。

## 可直接加入模型内部能力说明的摘要

需要可视化时，在普通 Markdown 内输出 `yukinal-visual` 围栏，内容必须为 JSON 对象：`{"version":1,"kind":"chart|plot|flow|scene3d","title":"简短标题",...}`。不要输出 JavaScript、SVG、HTML、远程 URL 或未定义字段。数值必须有限且绝对值不超过 1e9，每个围栏不超过 64 KB。可选 `description` 用于来源、假设、单位和图形说明，最长 2000 字符。可选颜色为 `blue|green|amber|purple|red|cyan`。优先选能清楚表达数据的最简单图形；给屏幕阅读器保留文字解释，数据不足时先说明缺项。

- `chart`: 可选 `style:"line"|"bar"`（默认 line）、`labels:string[]`、`series:[{label,values:number[],color?}]`、可选 `unit`。1–120 类别、1–6 系列，每条 values 与 labels 等长。折线按输入顺序相连；柱状图保留零基线及负数。
- `plot`: `series:[{label,points:[[x,y],...],mode?:"line"|"points",color?}]`、可选 `xLabel`,`yLabel`。1–6 系列，各 1–256 点。默认 line，点按给定顺序连线；不求值函数表达式。
- `flow`: `nodes:[{id,label,x?,y?,color?}]`、`edges:[{from,to,label?}]`。1–24 节点，最多 64 连接；id 唯一，连接须引用现有节点且不能自连。x/y 必须同时给出，范围 0–100，省略则按输入顺序分三列布局。坐标表示图面百分比，不能直接使用屏幕像素。
- `scene3d`: `nodes:[{id,label,position:[x,y,z],shape?:"sphere"|"box",size?,color?}]`、`edges:[{from,to,label?}]`。1–48 节点，最多 96 连接；默认 sphere、size=1，size 范围 0.1–10。节点中心为三维坐标；球形为固定可读标记，size 调整标记大小，box 的 size 为坐标单位边长。客户端支持拖动旋转、键盘方向键、缩放和复位。

通用标题与系列标签最长 160 字符；类别/节点标签、id、连接说明、坐标轴标签最长 80 字符；unit 最长 32 字符。所有字段严格校验，未知字段会拒绝整个文档，避免无声截断数据改变含义。多个图使用多个围栏。所有图包含可展开的完整数据表或节点/连接文字，长图和数据在控件内滚动。

## 示例

### 趋势与对比

```yukinal-visual
{"version":1,"kind":"chart","title":"CPU 与内存趋势","description":"示例数据，非真实监控读数；横轴为采样时刻。","style":"line","labels":["10:00","10:01","10:02","10:03"],"unit":"%","series":[{"label":"CPU","values":[23,46,31,20],"color":"blue"},{"label":"内存","values":[45,46,47,46],"color":"green"}]}
```

改为 `"style":"bar"` 可展示分组柱状对比。同一图各系列应使用相同量纲；不同单位请拆图。

### 二维坐标

```yukinal-visual
{"version":1,"kind":"plot","title":"y = x² 的采样点","xLabel":"x","yLabel":"y","series":[{"label":"x²","mode":"line","points":[[-2,4],[-1,1],[0,0],[1,1],[2,4]]}]}
```

### 关系与流程

```yukinal-visual
{"version":1,"kind":"flow","title":"只读检查流程","nodes":[{"id":"request","label":"用户请求","x":0,"y":50},{"id":"inspect","label":"单助手检查","x":50,"y":50},{"id":"reply","label":"整理结果","x":100,"y":50}],"edges":[{"from":"request","to":"inspect","label":"读取状态"},{"from":"inspect","to":"reply","label":"返回证据"}]}
```

### 三维信息场景

```yukinal-visual
{"version":1,"kind":"scene3d","title":"服务依赖的三维分层","description":"示意拓扑；位置是信息布局，不代表物理距离。拖动可旋转。","nodes":[{"id":"web","label":"Web","position":[-2,1,0],"shape":"box","color":"blue"},{"id":"api","label":"API","position":[0,0,0],"shape":"box","color":"green"},{"id":"db","label":"数据库","position":[2,-1,1],"shape":"sphere","color":"purple"}],"edges":[{"from":"web","to":"api","label":"HTTP"},{"from":"api","to":"db","label":"查询"}]}
```

## 流式输出与边界

不完整 JSON 在流式阶段显示“正在准备可视化”，不尝试修复或执行片段；一旦完整且有效便显示图形。最终无效或超限内容显示明确反馈并保留可检查的原文（超限原文仅显示前 64 KB）。错误文档不会触发远程请求。颜色来自固定色板，SVG 元素由客户端代码创建，模型文本只作转义后的文本节点。

当前 3D 是本地 SVG 正交投影的信息场景，并非 CAD、游戏或任意模型加载器；节点较多时标签可能互相遮挡，旋转、缩放及完整文字表可辅助阅读。流程图自动布局为固定三列，可用 x/y 明确安排分支。没有持续动画、隐藏渲染循环或后台采样。

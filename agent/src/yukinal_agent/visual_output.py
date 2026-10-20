"""Private model-facing presentation contract; never sent as public metadata."""

VISUAL_SELECTION_GUIDE = """用户要求画图、二维/2D、三维/3D 时，直接生成下述可渲染图形，不只描述如何绘制，也不输出绘图库代码代替图形。
二维趋势/对比选 chart，二维坐标/函数采样选 plot，关系/流程选 flow；三维坐标、空间关系、结构分层选 scene3d。
即使用户没有说“画图”，图比文字更清楚时也可使用；普通单事实、简单操作不强行配图。三维信息场景支持旋转和缩放，不宣称是 CAD、真实仿真或已加载任意外部模型。
只使用真实给定或已验证的数据；示意坐标、假设或函数采样在 description 中说明。不要把示意布局当作实测。
图形前后只保留必要结论，不重复解释可直接从图中读出的内容。"""

VISUAL_OUTPUT_GUIDE = '需要可视化时，在普通 Markdown 内输出 `yukinal-visual` 围栏，内容必须为 JSON 对象：`{"version":1,"kind":"chart|plot|flow|scene3d","title":"简短标题",...}`。不要输出 JavaScript、SVG、HTML、远程 URL 或未定义字段。数值必须有限且绝对值不超过 1e9，每个围栏不超过 64 KB。可选 `description` 用于来源、假设、单位和图形说明，最长 2000 字符。可选颜色为 `blue|green|amber|purple|red|cyan`。优先选能清楚表达数据的最简单图形；给屏幕阅读器保留文字解释，数据不足时先说明缺项。\n\n- `chart`: 可选 `style:"line"|"bar"`（默认 line）、`labels:string[]`、`series:[{label,values:number[],color?}]`、可选 `unit`。1–120 类别、1–6 系列，每条 values 与 labels 等长。折线按输入顺序相连；柱状图保留零基线及负数。\n- `plot`: `series:[{label,points:[[x,y],...],mode?:"line"|"points",color?}]`、可选 `xLabel`,`yLabel`。1–6 系列，各 1–256 点。默认 line，点按给定顺序连线；不求值函数表达式。\n- `flow`: `nodes:[{id,label,x?,y?,color?}]`、`edges:[{from,to,label?}]`。1–24 节点，最多 64 连接；id 唯一，连接须引用现有节点且不能自连。x/y 必须同时给出，范围 0–100，省略则按输入顺序分三列布局。坐标表示图面百分比，不能直接使用屏幕像素。\n- `scene3d`: `nodes:[{id,label,position:[x,y,z],shape?:"sphere"|"box",size?,color?}]`、`edges:[{from,to,label?}]`。1–48 节点，最多 96 连接；默认 sphere、size=1，size 范围 0.1–10。节点中心为三维坐标；球形为固定可读标记，size 调整标记大小，box 的 size 为坐标单位边长。客户端支持拖动旋转、键盘方向键、缩放和复位。\n\n通用标题与系列标签最长 160 字符；类别/节点标签、id、连接说明、坐标轴标签最长 80 字符；unit 最长 32 字符。所有字段严格校验，未知字段会拒绝整个文档，避免无声截断数据改变含义。多个图使用多个围栏。所有图包含可展开的完整数据表或节点/连接文字，长图和数据在控件内滚动。'

# Agent 回复的 Markdown 渲染

Agent 面板过去把模型回复当纯文本铺在动态里（`# 结论`、`- 一条`、`| 列 |` 都是原样显示的字符）。把它渲染成 Markdown 意味着要在渲染进程里处理**不可信文本**（模型输出可含任意内容，且会被 `filesystem.read` 之类的工具结果间接影响）；常见做法是 Markdown 库加 `dangerouslySetInnerHTML` 加净化库，那是「**默认放过、列举禁止**」：漏掉一条规则就是一个洞，而每加一个语法特性都要重新审视那张表。窗口侧也不直接生成可导航 `<a>`：桌面端只授予 opener 的默认 URL scope，点击由系统浏览器处理，WebView 自己不会离开应用。

**决定：Markdown 由仓库自己的解析器处理，并且只产出数据；渲染只创建我们自己写的元素。** 解析器在 `apps/desktop/src/lib/markdown/`（`markdown.ts` 是一个转发入口，实现按行内与块级拆成 `inline.ts` / `block.ts`；纯函数，输入文本、输出块与行内结构，不依赖 React、不依赖 DOM，因此可单独测试）；渲染在 `apps/desktop/src/components/MarkdownText.tsx`。

- **没有任何 `dangerouslySetInnerHTML`** —— 正文里的 HTML 是普通文字（`<script>` 显示成 `<script>`），因为它永远不会被当成标记解析，也就没有一张净化规则表需要维护对错。
- **链接只交给受限 opener** —— 只有 `http`/`https`/`mailto`/页内锚点会被认成链接；渲染为按钮并在外部打开，绝不生成会把 WebView 导航走的 `<a href>`。其余 scheme（`javascript:`、`data:`、`file:`）整段当普通文本。
- **远程图片必须得到用户明确同意** —— `![alt](https://…)` 默认渲染成文字按钮，不包含 `<img>` 或 `src`，所以不会自动发出请求。用户点击后创建的图片使用 `loading="lazy"` 与 `referrerPolicy="no-referrer"`。
- **支持的是子集，而且刻意选过**：ATX/setext 标题、`-`/`1.` 列表（可嵌套、可勾选，并按 CommonMark 区分紧凑/宽松）、围栏与四空格缩进代码块、引用、表格、分隔线、段落、引用式链接、脚注，以及行内代码/粗体/斜体/删除线/链接/裸 URL/数字字符引用。段落是整段解析的，所以代码段与强调可以跨软换行。**未闭合的围栏按「流式到这里为止」处理**，因为流式输出里它一定闭不上。HTML 仍按纯文本显示。
- **关键词着色在正文里全量保留**：Markdown 只负责结构，什么词是错误、路径还是标识符仍由原来的 token 规则决定，与日志页和工具卡片是同一套判断。
- **选中策略同时反转**：只有「你与 Agent 的对话正文」可以拖动选中，界面其余部分都不参与选择。做法是在 `.app-shell` 上关掉 `user-select`、只在对话正文上打开——`user-select` 是继承属性，不给每个容器补一条 `none` 才不会有「下一个加的元素忘了写」这种洞。

**代价**：我们自己维护这个解析器，只认对话正文需要的那一小撮语法，所以 CommonMark 的其余边角行为不承诺完全一致；列表紧凑/宽松与引用 lazy continuation 已有专门行为测试。未支持的语法按纯文本显示。

## 与 CommonMark 0.31.2 的差距（实测，2026-09-15）

**规范版本固定为 CommonMark 0.31.2**：这一版就是本文件与 `apps/desktop/tests/markdown-spec.test.ts` 里那个基线所对照的版本。升级规范版本要重新测量并更新基线，而不是顺手改数字。

**怎么测的。** 语料是官方 `https://spec.commonmark.org/0.31.2/spec.json`（652 个例子）。它**不进仓库**：那是 CC-BY-SA 的文本，而门禁是离线的，所以它是显式 opt-in 的一次测量，与 live provider / live MCP 测试同一套约定：

```powershell
curl.exe -o "$env:TEMP\commonmark-0.31.2.json" https://spec.commonmark.org/0.31.2/spec.json
$env:YUKINAL_MARKDOWN_SPEC = "$env:TEMP\commonmark-0.31.2.json"
pnpm --filter @yukinal/desktop test
```

每个例子跑两个指标，都会打印成表格：

- **identical**：我们的纯文本与规范 HTML 的纯文本（去标签、解实体、折叠空白）**完全相等** —— 这是「按规范解释了这个结构」。
- **kept**：规范页面上的每一个字符都按顺序出现在我们的文本里（允许我们**多**出未解释的标记字符）。这一列才是「没认出来的东西一个字都不能丢」。

两个指标都是**下限**：测试断言的是「不许变差」，不是绝对值。修好一处，数字会涨；让它降下来则是一次回归。

**哪些例子算「在范围内」。** 652 个例子里有 **189 个被排除**，因为差异出在我们**有意**的行为上，而不是解析器上：

- HTML 块与行内 HTML 按纯文本显示（`HTML blocks`、`Raw HTML` 两节整节排除）；
- 链接只认 `http(s)`、`mailto` 与页内锚点，规范语料里大量使用相对目标 `/uri`（这类例子排除，`Images` 的 `src` 同理）；
- 图片默认不下载；只有用户显式点击“加载远程图片”后才会请求资源。

排除是**逐例、按预期 HTML 判断**的，不是按节一刀切，而且每次运行都会打印排除数：排除规则变了，`total` 就会变，而测试要求 `total` 与基线一致——范围一动就必须有人明确地重新基线，不能悄悄缩表。

**当前实测：范围内 463 个例子里 434 个 identical、461 个 kept（2 个丢字）。** 各节（括号里是文档声明支持的范围）：

| 节 | identical | kept |
| --- | --- | --- |
| ATX headings（支持） | 17/18 | 18/18 |
| Autolinks | 13/15 | 15/15 |
| Backslash escapes（支持） | 8/10 | 10/10 |
| Block quotes（支持） | 24/25 | 25/25 |
| Code spans（支持） | 21/21 | 21/21 |
| Emphasis and strong emphasis（支持） | 123/123 | 123/123 |
| Entity and numeric character references | 12/14 | **12/14** |
| Fenced code blocks（支持） | 29/29 | 29/29 |
| Hard line breaks | 13/13 | 13/13 |
| Images | 1/2 | 2/2 |
| Indented code blocks（支持） | 12/12 | 12/12 |
| Link reference definitions（支持） | 6/10 | 10/10 |
| Links（支持） | 16/24 | 24/24 |
| List items | 47/48 | 48/48 |
| Lists（支持） | 23/26 | 26/26 |
| Setext headings | 23/27 | 27/27 |
| Tabs / Paragraphs / Thematic breaks / Blank lines / Inlines / Precedence / Soft line breaks / Textual content | 全对 | 全对 |

**加粗的换行数是「真的丢字」**（现在只剩 2 个例子）：页面上的内容没有出现在我们的输出里。已经修掉的五类（第一轮实测 72 个丢字 → 29 → 2）：

1. **反引号 run 必须**等长**才算闭合。** 之前用 `indexOf` 找结束标记，更长的 run 会被当成命中（`` ` `` `` ` `` `` ` `` 被切成两个空代码段）。现在逐 run 比较长度。
2. **有序列表只有从 1 开始才能打断段落。** `14. The number of doors is 6.` 是句子不是列表；之前它会被当成列表，`14.` 那个标记连同数字一起消失。空列表项同样不再打断段落。
3. **强调按规范的 delimiter run 算法配对。** 扫描时只记录 `*`/`_` run 的左右贴边（标点用 `\p{P}\p{S}`，规范里 `$`/`£`/`€` 也算标点），扫描完再按第 9/10 条（「3 的倍数」）与第 13–17 条配对。之前在扫描时就往回找结束标记，`a**"foo"**`、`*foo**bar*`、`foo__bar__`、`пристаням_стремятся_` 这类例子会被配成规范不认的强调，标记因此从正文里消失。这一条把 Emphasis 从 106/123 拉到 123/123。
4. **容器里的「续行」有了自己的身份（lazy continuation line）。** 缩进不到容器要求的行只剩正文：`- a` / ` - b` / `  - c` / `   - d` / `    - e` 的最后一行不再变成第五层列表，`1. a` / `  2. b` / `    3. c` 的最后一行落到列表外面由四空格缩进代码接住，引用里 lazy 的 `===` 也不再变成 setext 标题。
5. **段落整段解析。** 换行因此也是行内语法：`` `code\ `` 换行 `` span` `` 是一个代码段（行尾反斜杠在代码段里是字符），强调、链接也能跨软换行；段落最后一行结尾的反斜杠则按规范留作普通文字。

**还剩下的 2 个丢字是有意的偏差，不是待修的缺口**：`Entity and numeric character references` 里的 **#25 与 #41** 用的是**命名**引用（`&copy;`、`&quot;`）。数字引用（`&#35;`、`&#x22;`，含 `U+0000` 与非法码位换 `U+FFFD`）已经实现；命名引用要一张 2231 条的 HTML5 名字表（`https://html.spec.whatwg.org/entities.json`），那是几十 KB 的纯数据，与「不为小功能引大依赖」的取舍冲突，所以按字面显示。这条偏差是**可测的**：基线里 `14 个例子 12 个 identical / 12 个 kept` 就是它，数字一动就会被门禁抓住。

**有意的偏差（不是缺口）**：HTML 一律按纯文本显示、链接不生成可导航 `<a>`、远程图片在用户明确同意前保持惰性。这三条让 `HTML blocks` / `Raw HTML` / `Images` 的 identical 数天生低——规范把它们变成标记，我们让它们留在页面上。kept 列才是这些节该看的数字。

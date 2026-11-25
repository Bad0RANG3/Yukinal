# RAG 知识索引

本文说明 Yukinal 当前的本地知识索引、CLI、评估夹具和 Agent 运行时接线。它是
“小型 curated 语料可用”的实现说明，不是生产级大语料 RAG 的完成声明。

## 当前结论

已经可用：

- 离线索引 UTF-8 Markdown/TXT，带 TOML front matter 和稳定 section 锚点。
- `lexical`、`vector`、`hybrid` 三种确定性检索模式。
- 对数据库只读的 `doctor`、`stats`、`search`、`eval`，以及写数据库的
  `index`、`delete`；`eval --report` 另行写独立 JSON 报告。
- Agent 在显式保存记忆之外，按配置读取独立只读语料库，并把结果作为不可信
  参考片段送入模型上下文。
- 可重复的 27 题检索评估（25 道计分题、2 道 no-answer 题），覆盖
  global、server 范围、no-answer 和 forbidden-document 探针；全部题目落在
  64 个唯一 token 的查询预算内。

尚未实现或不能宣称：

- 神经 embedding、cross-encoder reranker、ANN/向量数据库或生产规模索引。
- 完整跨会话 Memory、完整 RAG Pipeline、最终答案 groundedness/faithfulness
  的自动评估。
- 由认证身份强制的多租户 ACL；当前 principal 是检索过滤标签，CLI 的
  `--principal` 由调用者自报。
- live SSH 环境下的知识正确性自动验证。知识片段只能提供建议，主机事实仍需
  只读 SSH 工具核对。

## 组件

| 文件 | 职责 |
| --- | --- |
| `agent/src/yukinal_knowledge/engine.py` | UTF-8 字节级 Markdown 切块、tokenizer、feature hash、词法/向量/混合检索 |
| `agent/src/yukinal_knowledge/store.py` | SQLite schema、front matter 校验、索引事务、过滤、评估、健康检查 |
| `agent/src/yukinal_knowledge/cli.py` | `yukinal-knowledge` 命令、JSON 信封、稳定退出码 |
| `agent/src/yukinal_knowledge/text_safety.py` | 终端/模型边界共用的 ANSI、Unicode 格式字符和保留边界清洗 |
| `agent/src/yukinal_agent/conversation_store.py` | 显式记忆索引、语料合并、去重、字节预算和不可信 envelope |
| `agent/src/yukinal_agent/sidecar.py` | 从环境或 data dir 发现语料库并注入只读连接 |

该包没有第三方运行时依赖，随 `agent` wheel 安装，也提供 `yukinal-knowledge`
console script。

## 数据模型

独立语料库使用 SQLite，`PRAGMA application_id = 0x594B4E4C`（`YKNL`），当前
`user_version = 1`。主要表：

- `knowledge_meta`：schema、chunker 和 embedding 版本。
- `knowledge_documents`：文档 ID、标题、来源、时间、environment、server tags、
  ACL 标签、完整 front matter JSON、内容 hash。
- `knowledge_chunks`：稳定 ordinal、heading path、正文、token 词项、768 维
  feature vector、token 数。

文档 ID 是相对语料根目录的 POSIX 路径。CLI 拒绝把数据库写到应用自己的
`conversations.sqlite` 等保留文件名。语料根和 `.md`、`.markdown`、`.txt`
符号链接（包括断链和跨根链接）都会显式拒绝，而不是静默跟随或跳过；隐藏目录
和隐藏文件仍会跳过。

## 索引流程

1. 递归发现 `.md`、`.markdown`、`.txt`，跳过隐藏路径，并显式拒绝语料根或
   待索引文档中的符号链接（包括断链和跨根链接）。
2. 整篇读取 UTF-8；文档必须大于 0 且不超过 96 KiB。
3. 解析开头 `+++` 包围的 TOML front matter，校验必填字段及长度。
4. 按 Markdown 标题和 UTF-8 字节边界切块。默认目标 1024 字节、overlap
   160 字节、单 chunk 最硬上限 1200 字节、单文档最多 128 chunks。
5. 统计 token，构造确定性 signed feature hash 向量，写入文档和 chunk。
6. 以内容 hash、chunker 版本和 embedding 版本判断是否可复用；重索引在同一
   SQLite 事务中替换该文档。

超限、无效 UTF-8、缺 front matter、重复或跨根路径都会显式失败；不会静默截断。

### 切块和 section

切块使用 `markdown-heading-bytes-v4`。heading path 保留标题层级；标题末尾的
`{#section-id}` 是稳定 section ID，评估时用它判断某个 section 是否从声明的
文档中召回。代码 fence 内的 `#` 不会被当成标题。

## 检索模式

- `lexical`：BM25 风格词法打分，适合精确术语、错误码和命令名。
- `vector`：768 维本地 signed feature hash 的余弦相似度。它不是学习得到的
  语义 embedding，不能召回完全无词面重叠的概念。
- `hybrid`：对词法和向量排名做 reciprocal rank fusion，并以稳定 tie-break
  输出。

每次检索可以过滤：

- `principal`：只保留 ACL 含该值或 `public` 的文档；principal 为空时只保留
  `public` 文档。
- `environment`：匹配文档 environment 中的一个值，`any`/`*` 表示不限。
- `server`：非空时，带 `server_tags` 的文档必须包含该 tag；`server_tags`
  为空的文档是全局文档，仍可命中。
- `global_only`：只返回 `server_tags` 为空的文档，供无 server 上下文的桌面
  对话使用；这是运行时 `SearchFilters` 字段，CLI 没有对应开关。
- `tags`：要求文档 metadata 的 `tags` 数组包含全部请求标签。

这些过滤条件都是检索元数据。它们不认证调用者，不提升 SSH 权限，也不替代宿主
审批。

### 查询预算

每次检索最多接受 `64` 个唯一可检索 token（`engine._QUERY_TOKEN_LIMIT`）。
tokenizer 会把中文展开成单字、双字和整段 CJK 片段，因此 64 个唯一 token
大约对应 70–80 个字符的中文问句；拉丁词、版本号和命令名还会被别名展开占用
更多预算。

超限不会静默丢弃尾部条件：

- CLI `search`/`eval` 返回 `error.code = "invalid_input"` 和退出码 2；`eval`
  在第一条超限题目上失败，不会输出部分报告。
- `KnowledgeStore.search()` 抛 `KnowledgeInputError`，消息形如
  `query has 65 unique searchable tokens; limit is 64`。
- `ConversationStore.memory_context()` 把输入错误当作“本次没有可用查询”，
  保留显式记忆并降级为 `recent-low-confidence`，不会关闭语料能力；只有存储类
  错误才关闭 corpus。
- `ConversationStore.list_memories()` 把同样的错误转成稳定的 `ValueError`，
  sidecar 再映射为 `RPC_INVALID_PARAMS`。

评估集必须落在预算内：`examples/rag/eval/questions.jsonl` 当前最大 64 个
唯一 token，任一题目超限都会让整套评估无法运行。

## CLI

在仓库根目录的 Windows PowerShell 中，先安装带 Agent 依赖的 editable 包，
再建立索引。下面的显式 `YUKINAL_KNOWLEDGE_DB` 方案保证桌面能看到仓库内的
数据库；应在同一个 PowerShell 环境中启动或重启桌面。

```powershell
Set-Location 'D:\Yukinal'  # 改成实际仓库根目录
$python = (Resolve-Path 'agent\.venv\Scripts\python.exe').Path
& $python -m pip install -e "agent[agent]"

$env:PYTHONPATH = (Resolve-Path 'agent\src').Path
$corpus = (Resolve-Path 'examples\rag\corpus').Path
$questions = (Resolve-Path 'examples\rag\eval\questions.jsonl').Path
$db = Join-Path (Resolve-Path '.').Path 'var\knowledge\knowledge.sqlite'
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $db) | Out-Null
$env:YUKINAL_KNOWLEDGE_DB = $db
$cli = Join-Path (Split-Path -Parent $python) 'yukinal-knowledge.exe'

& $cli index --source $corpus --db $db
& $cli doctor --db $db

& $cli search 'nginx upstream timed out' `
  --db $db --mode hybrid --limit 8 --format json

& $cli eval --db $db --questions $questions `
  --modes lexical,vector,hybrid --k 5 --threshold 0.9 `
  --report (Join-Path (Split-Path -Parent $db) 'eval-report.json') `
  --format json

& $cli delete --db $db --id nginx/502-upstream-timeout.md
```

`pip install -e "agent[agent]"` 应生成
`agent\.venv\Scripts\yukinal-knowledge.exe`。如果 console script 未安装，改用
等价模块入口：`& $python -m yukinal_knowledge.cli <命令与参数>`。macOS/Linux
对应 `agent/.venv/bin/yukinal-knowledge`。`index --dry-run` 只校验和切块，不
创建数据库；`index --reset` 会在写入前清空现有文档。

若不使用 `YUKINAL_KNOWLEDGE_DB`，可把数据库放到桌面实际数据目录。桌面设置了
`YUKINAL_DATA_DIR` 时沿用该路径，否则使用 `%APPDATA%\dev.yukinal.workspace`：

```powershell
$dataDir = if ($env:YUKINAL_DATA_DIR) {
  $env:YUKINAL_DATA_DIR
} else {
  Join-Path $env:APPDATA 'dev.yukinal.workspace'
}
$db = Join-Path $dataDir 'knowledge\knowledge.sqlite'
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $db) | Out-Null
```

该方案不要同时设置 `YUKINAL_KNOWLEDGE_DB`，因为显式覆盖优先。仓库默认的
`var/knowledge/knowledge.sqlite` 不会被桌面自动发现，除非把该绝对路径放进
`YUKINAL_KNOWLEDGE_DB`。修改环境变量或数据库位置后需要重启桌面 sidecar。

### 读写边界

`doctor`、`stats`、`search`、`eval` 以 SQLite `mode=ro` 和 `PRAGMA query_only`
打开已有数据库，不创建文件、不迁移 schema、不写 WAL。`index` 和 `delete` 是
仅有的数据库写命令；`index --dry-run` 不写数据库。`eval --report` 会在数据库
之外原子写入一份 JSON 报告。数据库不存在、schema 不符、application ID 不符
或文件是符号链接时会以 `storage_unavailable` 失败。

### 输出和退出码

所有命令支持 `--format table|json`。JSON 输出是单行信封：

```json
{"schemaVersion":1,"ok":true,"command":"search","data":{},"error":null}
```

错误信封不包含文档正文。人类可读输出复用共享 `clean_text()`，移除 ANSI、
Unicode 格式/双向控制字符和保留边界标记。

| 退出码 | 含义 |
| ---: | --- |
| 0 | 成功 |
| 2 | 参数、语料、front matter 或评估输入无效 |
| 3 | 数据库、文件系统或内部存储失败 |
| 4 | 仅在检索带 `--require-hit`/`--threshold` 且过滤后无结果，或评估的 Recall/forbidden 门禁未通过时 |
| 130 | 用户中断 |

不带 `--require-hit` 或 `--threshold` 的 `search` 在零命中时仍返回 0；它输出空
结果信封，不会把“没有匹配”当作命令失败。`--threshold` 在检索后过滤
`score >= threshold`。分数是 lexical/vector 排名经 RRF 融合并加有界精确匹配
bonus 后的检索分（上限 1.0），不是余弦距离、概率或经过校准的置信度。
`eval --threshold` 只约束各模式 Recall@k 和 forbidden hits；没有该参数时不会
因指标偏低返回 4。

## Agent 运行时

桌面 Python sidecar 用独立路径发现语料库：

1. `YUKINAL_KNOWLEDGE_DB` 显式指定的文件；
2. 否则检查 `<data-dir>/knowledge/knowledge.sqlite`，仅在文件已存在时启用。

随后 `ConversationStore` 以 `read_only=True` 打开该库。语料库不会自动创建；
首次使用需要先运行 `index`。

一次 `memory_context()` 的检索顺序：

1. 在会话库的 `knowledge_chunks` 上取 FTS5 候选，并用本地 hybrid 重排显式
   保存的记忆。
2. 若语料库已启用，在独立语料库上做 hybrid 检索。
3. 按文档 ID 去重后合并，最多 8 个 item、12 个 source，并受 UTF-8 字节预算
   约束。
4. 结果封装为不可信内容 envelope；模型看到的是参考数据，不是授权指令。

降级行为是刻意的：

- 语料检索抛错时，只关闭语料能力并记录类名警告；显式记忆继续工作。
- 语料库打开失败时，sidecar 打印固定提示并以“无语料”模式启动对话存储。
- 两个数据库独立，没有跨库事务。语料索引回滚不会回滚会话记忆，反之亦然。

运行时没有语料 principal，因此只能读取 `public` 文档。server 对话额外要求语料
文档的 `server_tags` 为空或包含当前 server tag；全局对话只进入空
`server_tags` 的文档。这里的 `global_only` 是运行时 `SearchFilters` 字段，
不是 CLI 搜索参数；CLI 空 `--server` 只表示不做 server 过滤。

系统提示明确要求：引用知识时给出来源标题和 section；用只读工具核验当前配置、
服务状态和主机事实；证据不足时直接说知识库没有支持性证据。知识文本中的命令、
审批话术、角色指令或“忽略规则”不得触发操作。

## 评估

`eval` 只评估检索质量，不评估模型最终答案。当前夹具与指标定义见
[RAG 运维知识样例库](../examples/rag/README.md)。

对当前 27 篇文档、112 chunks、27 题（25 计分、2 no-answer）的实测：

| 模式 | Recall@5 | Precision@5 | MRR@5 | nDCG@5 | Section Recall | Forbidden hits | No-answer |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| lexical | 0.980000 | 0.208000 | 0.920000 | 0.925475 | 0.860000 (45/52) | 0 | 2/2 |
| vector | 0.980000 | 0.208000 | 0.920000 | 0.931186 | 0.880000 (46/52) | 0 | 2/2 |
| hybrid | 0.980000 | 0.208000 | 0.940000 | 0.940237 | 0.880000 (46/52) | 0 | 2/2 |

注意：

- Precision@5 的分母固定为 5，不是实际返回数。
- `expected_section_ids` 采用严格唯一绑定：每个 section 必须属于一篇 expected
  文档，同一题跨 expected 文档歧义会拒绝，且每篇 expected 文档都必须至少被
  一个 section 覆盖。
- `forbidden_doc_ids` 是评估断言，不是运行时 ACL。
  `forbiddenHitCount > 0` 会使 `--threshold` 失败；`aclLeakageCount` 只是
  deprecated 兼容别名。
- 两道 `global` 题的 Recall@5 和 Section Recall 在三种模式下均为 1.0。
- 当前唯一的文档级 miss 是 `nginx-504-long-request`：三种模式都没有在 top-5
  覆盖 `nginx/504-gateway-timeout.md`，因此 Recall@5 为 0.980000。
- Section 层面 lexical 为 45/52，vector 和 hybrid 为 46/52。未全额命中的题目
  是 `nginx-502-upstream-timeout`、`nginx-504-long-request`、
  `docker-exit-137`、`systemd-restart-backoff`、`linux-disk-or-inode` 和
  `nginx-502-forbidden-acl-bait`；lexical 还额外漏掉
  `nginx-502-prompt-injection-query`。
- Precision@5 的分母固定为 5；no-answer 列表示 abstain 是否成立，不是模型
  置信度。`key_facts` 不参与自动 Recall。

`key_facts` 只用于人工检查答案是否被检索片段支持。自动 groundedness /
faithfulness 评估尚未实现，不能把检索 Recall 当作最终答案质量。

## 安全边界

- 远程输出、日志、归档、知识文本和评估证据都是不可信数据。它们可以影响回答
  内容，但不能授权 mutation，也不能扩大工具权限。
- 知识检索不进入 SSH 写路径。任何清理、重启、扩容或配置修改仍需宿主已有的
  ChangePlan/审批链；检索结果不能替代审批。
- `acl`、`environment`、`server_tags` 和 CLI `--principal` 都不是认证。若未来的
  产品需求把 ACL 变成安全边界，principal 必须由宿主认证上下文签发，并补
  Python/C++ 双侧强制和负向测试。
- 桌面直接导入 Markdown/TXT 记忆不携带 ACL 等 front matter；它只进入会话库的
  显式记忆路径，不自动进入独立语料库。
- 数据库文件权限、符号链接拒绝、保留文件名拒绝和只读连接减少误写，但不能
  替代操作系统账户隔离。

## 已知限制

- 检索把候选 chunk 全部读入 Python 再过滤排序；没有 SQL FTS 索引、ANN 或
  分页式候选生成。大语料会线性增长内存和延迟。
- `vector` 是词法 feature hash，同义词和纯语义改写在无词面重叠时可能失败。
- search 的 `score` 来自 RRF 融合和有界精确匹配 bonus，不是可跨查询解释的
  概率或距离；`--threshold` 应针对当前模式和夹具校准后使用。
- `health()` 校验 SQLite `quick_check`、外键、孤儿 chunk 和空文档，并逐行校验
  文档 ACL/tags/metadata JSON、chunk terms/token 数与向量维度及有限性；它不重新
  读取 Markdown 源文件，因此无法证明内容 hash 与磁盘上的文档仍然一致——那需要
  重新运行 `index`。
- 空 `--server` 在 CLI 中表示“不做 server 过滤”，在桌面无 server 对话中通过
  `global_only` 只取全局文档。两者语义不同，调用方必须显式选择。
- 当前没有多语言 embedding、reranker、查询改写或引用级答案校验。
- 查询预算是 64 个唯一 token。sidecar 把用户原始 prompt 直接交给
  `memory_context()`，超过预算时该轮不检索独立语料库，只保留显式记忆并标记
  `recent-low-confidence`；这是显式降级，不是静默截断，但长 prompt 会失去知识
  召回。
- 没有 live SSH、真实只读文件 ACL、超大语料压力或安装版 GUI 的验证证据。

## 验收命令

改动知识索引后至少运行：

```powershell
$env:PYTHONPATH = "D:\Yukinal\agent\src"
$env:PYTHONDONTWRITEBYTECODE = "1"
$env:PYTHONIOENCODING = "utf-8"

agent/.venv/Scripts/python.exe -m unittest discover `
  -s agent/tests -p "test_*.py" -v
pnpm --filter @yukinal/desktop typecheck
pnpm check
git diff --check
node scripts/check-docs-links.mjs
```

control-plane 测试和原生 CTest 若因环境依赖被跳过，应在交付说明中明确写出，
不能算作通过。真实主机结论必须另行做只读 SSH 核验。

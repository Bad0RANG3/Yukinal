# RAG 知识索引交接

更新日期：2026-11-25

仓库：`D:\Yukinal`，基线提交 `9755f2d`，分支 `main`。

## 交接结论

本地 curated RAG 的索引、检索、CLI 和 Agent 运行时接线已经实现，并有一组可重复的
单元测试和集成测试。当前可以用于仓库内的小型运维知识语料，但**不能**按“成熟、
可直接投产的完整 RAG”验收（以下各点是接手时的状态，续做后的结论见本节日期
说明）：

- 检索仍是确定性的词法/feature-hash 方案，不是神经语义检索。
- 共享 context schema 尚未接受 Python 实际发出的 knowledge source，桌面同步或
  `agent.completed` 校验可能失败。
- 65 个唯一查询 token 会被 CLI 错报为 `internal_error`，而不是参数错误。
- `docs/rag-knowledge-index.md` 和 `examples/rag/README.md` 中仍有 v3 切块器和
  旧评估指标，必须在 v4 引擎上重跑后才能作为当前证据。
- 尚未完成 `pnpm check`、完整桌面实机验收、live SSH、超大语料和安装版 GUI 验收。

本目标目前未完成；本文是接手基线，不是完成声明。截至本文写入时，相关改动仍在
工作区中且未提交。

> **2026-11-25 续做状态**：上面两个已确认缺陷已修复，两份过期文档已在 v4 引擎
> 上重跑并更新，`pnpm check`、`git diff --check`、
> `node scripts/check-docs-links.mjs` 均已通过；native 构建与 CTest、control
> plane 测试因本机没有对应环境而跳过。仍未完成的是桌面实机验收。详见
> 「续做进展」一节。相关改动仍未提交。

## 已实现范围

### 知识包与 CLI

`agent/src/yukinal_knowledge/` 已提供：

- UTF-8 字节边界上的 Markdown/TXT 切块，保留稳定 heading path、section ID 和偏移。
- TOML front matter、ACL、environment、server tags 和 metadata 的解析与检索过滤。
- `lexical`、`vector`、`hybrid` 三种确定性检索；混合模式使用 reciprocal rank
  fusion 和稳定 tie-break。
- 独立 SQLite 知识库，校验 application ID、schema、chunker 版本和 embedding
  版本。
- `index`、`delete`、`doctor`、`stats`、`search`、`eval` 子命令。
- 单行 JSON 信封和稳定退出码；人类可读输出经过终端控制字符清洗。

当前关键常量：

| 项目 | 当前值 |
| --- | --- |
| `CHUNKER_VERSION` | `markdown-heading-bytes-v4` |
| `EMBEDDING_VERSION` | `signed-feature-hash-v2` |
| SQLite `user_version` | `1` |
| application ID | `0x594B4E4C` / `YKNL` |
| 单次查询唯一搜索 token 上限 | `64` |
| feature vector 维度 | `768` |
| 默认 chunk 参数 | 目标 1024 字节、overlap 160 字节、硬上限 1200 字节 |
| 单文档上限 | 96 KiB、最多 128 chunks |

### Agent 运行时

`agent/src/yukinal_agent/` 已完成以下接线：

- sidecar 优先读取 `YUKINAL_KNOWLEDGE_DB`；否则只在
  `<data-dir>/knowledge/knowledge.sqlite` 已存在时启用语料库。
- 语料库以只读 SQLite 连接打开，打开失败只关闭语料能力，不影响显式记忆和
  Agent turn。
- `memory_context()` 把会话库中的显式记忆与独立语料库结果合并、按文档去重，并受
  item、source 和 UTF-8 字节预算约束。
- 没有 principal 时只返回 `public` 文档；server 对话只接受全局文档或匹配当前
  server tag 的文档。
- 检索文本进入不可信内容 envelope，不能成为系统指令、授权证据或 mutation
  许可。
- 超长或零命中查询可回退到同作用域的最近显式记忆，并标记
  `recent-low-confidence`；该回退不应被当作检索命中。
- 桌面 `MemoryLibrary` 只导入显式记忆，不会自动写入独立知识语料库。

## 数据流

```text
Markdown/TXT 语料
  -> yukinal-knowledge index
  -> knowledge.sqlite（独立只读语料库）
  -> ConversationStore.memory_context()
  -> ACL / environment / server / global_only 检索过滤
  -> untrusted_tool_content(...) 有界 envelope
  -> 模型上下文中的参考片段

桌面保存的显式记忆
  -> conversations.sqlite
  -> FTS5 候选 + 本地 hybrid 重排
  -> 与语料库结果合并
```

两个数据库没有跨库事务。语料索引失败不会回滚显式记忆，反之亦然。桌面 UI
不直接连接知识库；它通过 Python sidecar 的 Agent 上下文读取结果。

## 本轮验证

以下结果于 2026-11-25 在 Windows PowerShell 中实际运行：

```powershell
$env:PYTHONPATH = "agent/src"
$env:PYTHONDONTWRITEBYTECODE = "1"
$env:PYTHONIOENCODING = "utf-8"

& agent\.venv\Scripts\python.exe -m unittest discover `
  -s agent/tests -p "test_knowledge*.py" -v
# Ran 78 tests; OK

& agent\.venv\Scripts\python.exe -m unittest `
  agent.tests.test_agent_conversation_context `
  agent.tests.test_knowledge_runtime `
  agent.tests.test_history_output_security -v
# Ran 36 tests; OK

pnpm --filter @yukinal/shared typecheck
# green

git diff --check
# green
```

这些证据只覆盖测试所描述的本地行为，不代表 `pnpm check`、桌面实机、live SSH
或生产规模已经通过。

## 已确认缺陷

### 1. 共享 context schema 不接受 knowledge source

严重度：高。可能影响 `agent_run_start` 响应、`agent.completed` 事件和桌面 UI
进入 completed 状态。

`packages/shared/src/schemas/agent-context.ts:22` 的
`AgentContextUsageSchema.memorySources` 目前只允许：

```ts
scope: z.enum(["global", "server"])
```

但 Python 在 `agent/src/yukinal_agent/conversation_store.py` 的
`memory_context()` 中会为语料库结果生成：

```json
{
  "id": "<doc_id>",
  "scope": "knowledge",
  "serverId": "<optional>",
  "title": "<title>",
  "updatedAt": "<timestamp>",
  "section": "<section>",
  "chunkId": "<doc_id#chunk-0000>"
}
```

共享 schema 会同时报 `scope` 枚举失败和 `section`/`chunkId` 的 unrecognized
keys。`packages/shared/src/types/chat.ts:35` 的 `AgentContextUsage` 也仍然使用
`AgentMemorySource[]`，与实际 payload 不一致。

推荐修法：

1. 保留 `AgentMemorySchema` 及 memory save/list/delete 的
   `global | server` 范围，不要扩大显式记忆 API。
2. 新增 context 专用的 `AgentKnowledgeSourceSchema`，严格要求
   `scope: "knowledge"`，并在该分支允许、校验非空的 `section` 和 `chunkId`。
3. 将 `AgentContextUsage.memorySources` 改为
   `Array<AgentMemorySource | AgentKnowledgeSource>`，不要给
   `AgentMemorySource` 本身增加 `knowledge`。
4. 为 `agent_run_start.response` 和 `agent.completed` 增加带真实 knowledge
   payload 的回归测试；只测 Python 端对象或单独测 schema 都不够。

状态（2026-11-25 续做）：已修复。`packages/shared/src/schemas/agent-context.ts`
现在使用 `AgentContextSourceSchema = z.union([AgentMemorySourceSchema,
AgentKnowledgeSourceSchema])`，knowledge 分支强制 `scope: "knowledge"` 且要求
非空 `section` 与 `chunkId`；`packages/shared/src/types/chat.ts` 同步导出
`AgentKnowledgeSource` / `AgentContextSource`。回归测试使用真实 producer 输出
固化的 fixtures（`packages/shared/fixtures/agent/context_knowledge_sources.json`、
`run_start_knowledge_sync.json`、`completed_knowledge_event.json`）：
`agent-context.test.ts` 覆盖 `agent_run_start` 与 `agent.completed`，
`agent/tests/test_knowledge_runtime.py` 重跑 `memory_context()` 并逐字段比对
fixture。`pnpm --filter @yukinal/shared test` 150 项通过。

### 2. 65-token 查询被映射为内部错误

严重度：中高。输入错误被错误归类为服务端故障，可能让 CLI 和 IPC 调用方无法
区分用户输入问题与存储损坏。

相关边界：

- `agent/src/yukinal_knowledge/engine.py:57` 定义 `_QUERY_TOKEN_LIMIT = 64`。
- `agent/src/yukinal_knowledge/engine.py:174` 的 `QueryTokenLimitError` 继承
  `ValueError`。
- `agent/src/yukinal_knowledge/store.py:1208` 的 `KnowledgeStore.search()` 直接
  调用检索，没有在存储边界转换该异常。
- CLI 只把 `KnowledgeError` 映射为稳定错误；其他异常落入
  `_InternalError`，因此 65 token 返回 `internal_error` 和 exit code 3。
- `agent/src/yukinal_agent/conversation_store.py:639` 的 `list_memories()` 直接
  调用 `_search_knowledge()`，可能把同类异常冒泡到 `agent.memory.list` IPC。

期望行为：

- 64 个唯一搜索 token 仍可检索。
- 65 个 token 返回 `error.code = "invalid_input"` 和 exit code 2。
- `memory_context()` 的 `ValueError` 降级路径继续保留；`list_memories()` 和
  sidecar `agent.memory.list` 需要有明确的稳定降级或参数错误策略，不能依赖
  外层碰巧捕获。

建议在 `KnowledgeStore.search()` 边界把 `QueryTokenLimitError` 转换为
`KnowledgeInputError`，并补 CLI 64/65 token、`memory_context()` 和
`agent.memory.list` 的回归测试。

状态（2026-11-25 续做）：已修复。`KnowledgeStore.search()` 在
`agent/src/yukinal_knowledge/store.py` 捕获 `QueryTokenLimitError` 并抛出
`KnowledgeInputError`，CLI 因此返回 `invalid_input` / exit code 2；
`conversation_store._search_corpus()` 先捕获 `KnowledgeInputError`，记警告并
返回空结果，不再把输入错误当存储损坏而永久关闭语料；`list_memories()` 把同类
异常转成稳定的 `ValueError`，sidecar `_memory_list` 再映射为
`RPC_INVALID_PARAMS`（同步与异步 dispatch 一致）。回归测试覆盖 64/65 边界
（`test_knowledge_store.py`、`test_knowledge_cli.py`）、`memory_context()`
降级到 `recent-low-confidence` 且 corpus 仍启用（`test_knowledge_runtime.py`）
以及 sidecar 两条 dispatch（`test_agent_conversation_context.py`）。

## 文档与评估的过期项

以下内容不能直接作为当前 v4 引擎的证据：

- `docs/rag-knowledge-index.md` 仍写 `markdown-heading-bytes-v3`。
- 同文档仍写 27 篇文档、112 chunks 和旧评估指标。
- 同文档仍称 `health()` 不校验 vector/terms/ACL/metadata；当前
  `health()` 已统计 `invalidDocuments` 和 `invalidChunks`，并通过
  `_storage_document_fields()`/`_storage_chunk_fields()` 解析 ACL、server
  tags、metadata、terms 和 vector。它仍不是完整的内容 hash 一致性审计。
- `examples/rag/README.md` 的 chunk 数量和 v3 说明已过期。
- `examples/rag/README.md` 的评估表来自旧引擎；其中的 lexical、vector、
  hybrid Recall/Section Recall 数字不能当作当前实现的结果。

接手后必须：

1. 用当前 v4 引擎在临时数据库上重新索引示例语料。
2. 运行 `stats` 记录实际文档/chunk 数。
3. 对 `lexical,vector,hybrid` 重新运行 27 题评估。
4. 用新报告的实际数字更新两份文档，并明确评估日期和运行平台。
5. 不要根据旧 v3 指标推断 v4 指标。

状态（2026-11-25 续做）：已更新。两份文档现在记录 v4 引擎在 Windows 上的实测：
27 篇文档 / 112 chunks、`chunkerVersion = markdown-heading-bytes-v4`、
`embeddingVersion = signed-feature-hash-v2`、`schemaVersion = 1`、
`doctor` 全部计数为 0。27 题评估（`--k 5 --threshold 0.9`）：
Recall@5 0.980000 / 0.980000 / 0.980000，Precision@5 0.208000，
MRR@5 0.920000 / 0.920000 / 0.940000，nDCG@5 0.925475 / 0.931186 /
0.940237，Section Recall 0.860000 (45/52) / 0.880000 (46/52) /
0.880000 (46/52)，forbidden hits 0，no-answer 2/2（lexical / vector /
hybrid）。唯一文档级 miss 是 `nginx-504-long-request`。另有 10 道评估题原先
超过 64 唯一 token 的查询预算，已缩短问句（`id`、expected、filters 未改），并
新增 `test_sample_evaluation_questions_fit_the_query_budget` 防止回退。

## 生产级边界

当前实现适合小型、人工维护、可审计的参考语料，不是通用生产 RAG：

- 没有神经 embedding、cross-encoder reranker、查询改写或 ANN/向量数据库。
- `vector` 是 signed feature hash；同义词和纯语义改写若没有词面重叠，可能
  无法召回。
- 检索把候选 chunk 全部读入 Python 再过滤和排序，没有 SQL FTS 分页候选生成
  或专用向量索引；大语料的内存和延迟会线性增长。
- search 分数是 RRF 融合分加有界精确匹配 bonus，不是概率、余弦距离或可跨
  查询解释的置信度。
- Precision@5 的分母固定为 5，不是实际返回文档数。
- 只评估检索指标；没有自动评估最终答案的 groundedness/faithfulness。
  `key_facts` 仍需人工或另建答案评估流程。
- `acl`、`environment`、`server_tags` 和 CLI `--principal` 是检索元数据。
  CLI 的 principal 由调用者自报，不是认证身份。
- 语料库和会话记忆库是独立 SQLite，没有跨库事务。
- 桌面直接导入 Markdown/TXT 不会携带 ACL/environment/server tags，也不会自动
  进入独立语料库。

## 安全边界

- 知识文本、检索片段、来源标题和评估报告都是不可信数据；它们可以影响回答，
  不能授权 mutation、扩大工具权限或替代宿主审批。
- 知识检索不得进入 SSH 写路径。任何清理、重启、扩容或配置修改仍需既有的
  ChangePlan、审批、结构化操作和 `shell_quote` 约束。
- 远程事实必须通过只读 SSH 工具核对；知识库不能证明当前主机状态。
- 若未来把 ACL 提升为真实安全边界，principal 必须由宿主认证上下文签发，
  并在 Python/C++ 两侧强制；不能继续使用 CLI 自报值。
- 语料不得包含凭据、私钥、密钥、真实内网地址或用户隐私。日志和评估报告输出
  也应遵守同一规则。
- 不可信 envelope 的截断结果不能被当作完整来源；只有完整编码进上下文的结果
  才能出现在 source 列表。

## 续做进展（2026-11-25）

接手顺序第 1–3 步已完成，第 4 步在本机通过但有三段环境缺失导致的跳过，第 6 步
已清理；第 5 步桌面实机仍未执行。

已完成：

- 共享 context schema、fixtures 与 Python producer 比对测试（见缺陷 1 状态）。
- 64/65 token 输入错误映射与四层回归测试（见缺陷 2 状态）。
- 临时库 v4 重跑：`index` 27 篇 / 112 chunks，`doctor` 全部计数 0，27 题
  `lexical,vector,hybrid` 评估 exit 0，两份文档已更新（见过期项状态）。
- 桌面渲染回归（非实机）：`apps/desktop/tests/agent-usage.test.tsx` 新增用例，
  断言 knowledge source 的标题出现在「使用记忆」中且不暴露 `chunkId`。这只能
  证明组件在静态渲染下接受新 scope，不能替代实机验收。

校验记录（本机 Windows，2026-11-25）：

- `pnpm check`：exit 0，`check: green`。其中 publication hygiene、secret
  scan、picomatch/命令 runner/release artifact/symlink/gate runner/docs anchor
  测试、docs links、`build contract libs`、`typecheck`、单元测试与 sidecar
  smoke 全部通过；Python Agent 全量 discovery 482 项通过（5 项
  `test_service_integration` 因未构建 C++ service 而跳过）；单元测试计数
  `@yukinal/shared` 150、`@yukinal/agent-sdk` 6、`@yukinal/provider-sdk` 4、
  `@yukinal/desktop` 653，均 0 fail；加入 knowledge source 渲染用例后
  `@yukinal/desktop` 为 654 且 typecheck 通过。
- `pnpm check` 中三段被跳过，不能算作已验证：native test build 与 CTest
  （`no configured CMake test build`）、control plane 测试（没有安装
  fastapi/sqlalchemy/alembic 的解释器）。
- `git diff --check`：green。
- `node scripts/check-docs-links.mjs`：`green (92 markdown files, 314 links)`。
- 临时评估库与报告、探针文件、`__pycache__` 已删除；仓库内 RAG 源码与测试
  保留。

仍未验证：桌面实机（knowledge source 显示、运行 completed、知识文本不触发
mutation）、native CTest、control plane 测试、live SSH、超大语料。

## 接手顺序

1. 修共享 context schema 与 TypeScript 类型，补 `agent_run_start` 和
   `agent.completed` 真实 payload 回归测试。（2026-11-25 已完成）
2. 修 64/65 token 输入错误映射，补 `KnowledgeStore.search()`、
   `memory_context()`、`list_memories()` 和 sidecar IPC 测试。（已完成）
3. 在临时数据库上用 v4 引擎重跑示例索引、`doctor`、`stats` 和三种模式评估，
   更新 `docs/rag-knowledge-index.md` 与 `examples/rag/README.md`。（已完成）
4. 运行完整校验；不要只跑 happy path。（本机已运行，结果见「续做进展」）
5. 在桌面实机中验证 knowledge source 能显示、运行能完成、知识文本不会触发
   mutation；保留真实证据后再讨论“可交付”。（未完成）
6. 清理仅由调查产生的临时数据库、探针文件和 `__pycache__`；不要删除仓库中
   尚未提交的 RAG 源码和测试。（已完成）

## 接手命令

### 目标测试

```powershell
$env:PYTHONPATH = "agent/src"
$env:PYTHONDONTWRITEBYTECODE = "1"
$env:PYTHONIOENCODING = "utf-8"

& agent\.venv\Scripts\python.exe -m unittest discover `
  -s agent/tests -p "test_knowledge*.py" -v

& agent\.venv\Scripts\python.exe -m unittest `
  agent.tests.test_agent_conversation_context `
  agent.tests.test_knowledge_runtime `
  agent.tests.test_history_output_security -v

pnpm --filter @yukinal/shared typecheck
```

### 临时 v4 评估

```powershell
$python = (Resolve-Path "agent\.venv\Scripts\python.exe").Path
$env:PYTHONPATH = (Resolve-Path "agent\src").Path
$corpus = (Resolve-Path "examples\rag\corpus").Path
$questions = (Resolve-Path "examples\rag\eval\questions.jsonl").Path
$db = Join-Path $env:TEMP "yukinal-rag-v4-eval.sqlite"
$report = Join-Path $env:TEMP "yukinal-rag-v4-eval.json"
Remove-Item -LiteralPath $db,$report -Force -ErrorAction SilentlyContinue

& $python -m yukinal_knowledge.cli index --source $corpus --db $db
& $python -m yukinal_knowledge.cli stats --db $db
& $python -m yukinal_knowledge.cli doctor --db $db
& $python -m yukinal_knowledge.cli eval `
  --db $db --questions $questions `
  --modes lexical,vector,hybrid --k 5 --threshold 0.9 `
  --report $report --format json
```

之后再把报告中的实际文档数、chunk 数和指标写回文档。临时文件不要提交。

### 最终校验

```powershell
pnpm check
git diff --check
node scripts/check-docs-links.mjs
git status --short
```

原生 CTest、桌面打包、安装版 GUI 和 live SSH 若没有实际运行，必须标记为未验证，
不能因为 Python 单测通过而推断通过。

## 未验证范围

- 真实主机的只读 ACL 和 SSH 事实核对。
- 10 万级 chunk、超大文档、并发检索和长期运行的性能/内存。
- 安装版桌面、真实 UI 事件链和 Windows 之外的平台。
- 神经 embedding、reranker、ANN 索引和答案级 groundedness。
- native 构建与 CTest（本机没有已配置的 CMake 测试构建目录，`pnpm check`
  明确跳过）、control plane 测试（本机没有安装 fastapi/sqlalchemy/alembic
  的解释器）。
- 桌面实机验收：knowledge source 显示、运行进入 completed、知识文本不触发
  mutation。

在这些项目获得独立证据之前，最准确的对外描述是：**本地小型 curated RAG
已实现并接入 Agent；两个已确认缺陷已修复，示例评估已在 v4 引擎上重跑，
本机自动化校验通过；桌面实机、native CTest 与生产规模仍未验证。**

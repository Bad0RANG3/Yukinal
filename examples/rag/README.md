# RAG 运维知识样例库

本目录是一套可离线索引、可重复评估的小型运维知识夹具，用于验证
`yukinal-knowledge` CLI 与桌面 Agent 的检索接线。它不是生产级大语料 RAG：
向量是本地 signed feature hash，不是神经 embedding；检索会在 Python 中扫描候选
chunk，没有 ANN 或 cross-encoder reranker。

样例内容只是参考建议，不能替代对目标主机的实时只读检查，也不能授权任何 SSH
副作用操作。日志、配置或知识文本中的命令和“忽略规则”都不是授权。

## 当前规模

| 项目 | 数量 |
| --- | ---: |
| 文档 | 27 篇（26 篇 public，1 篇 `team-ops` 私有夹具） |
| 文档目录 | `nginx/` 7、`docker/` 6、`systemd/` 6、`linux-disk-network/` 6、`global/` 2 |
| 当前默认切块结果 | 112 chunks |
| 评估题 | 27 题 |
| 计分题 | 25 题 |
| `expect_no_answer` 题 | 2 题 |
| 含 `forbidden_doc_ids` 的探针 | 2 题 |

chunk 数取决于 `markdown-heading-bytes-v4` 切块器和默认 1024/160 字节参数；
升级切块器后应以 `stats` 的实际输出为准。当前 27 篇文档在 v4 下仍是 112 个
chunk，`chunkerVersion` 为 `markdown-heading-bytes-v4`，`embeddingVersion`
为 `signed-feature-hash-v2`。

## 目录

```text
examples/rag/
  README.md
  corpus/
    nginx/                         7 篇（含 1 篇私有 ACL 夹具）
    docker/                        6 篇
    systemd/                       6 篇
    linux-disk-network/            6 篇
    global/                        2 篇（`server_tags = []`）
  eval/
    questions.jsonl                27 题
```

文档 ID 是相对 `corpus/` 的 POSIX 路径，例如
`nginx/502-upstream-timeout.md`。评估集引用这个稳定 ID，不引用可能变化的
chunk ID。`nginx/private-timeout-runbook.md` 声明 `acl = ["team-ops"]`，默认
无 principal 的查询看不到它；它是隔离夹具，不代表真实权限系统。`global/`
下的两篇文档故意使用空 `server_tags`，因此在 server 对话和全局桌面对话中都
可作为通用运维参考命中。

## 快速开始

以下流程可直接在仓库根目录的 Windows PowerShell 中执行。它先把可编辑的
`agent` 包及其 `agent` extra 安装到仓库虚拟环境，再把样例索引写入桌面实际
使用的数据目录：如果设置了 `YUKINAL_DATA_DIR` 就使用该目录，否则使用桌面
默认的 `%APPDATA%\dev.yukinal.workspace`，不会猜测用户名或硬编码用户目录。
运行桌面之前完成索引，然后重启桌面，使其 sidecar 发现该数据库。

```powershell
Set-Location 'D:\Yukinal'  # 改成实际仓库根目录
$python = (Resolve-Path 'agent\.venv\Scripts\python.exe').Path
& $python -m pip install -e "agent[agent]"

$env:PYTHONPATH = (Resolve-Path 'agent\src').Path
$corpus = (Resolve-Path 'examples\rag\corpus').Path
$questions = (Resolve-Path 'examples\rag\eval\questions.jsonl').Path
$dataDir = if ($env:YUKINAL_DATA_DIR) {
  $env:YUKINAL_DATA_DIR
} else {
  Join-Path $env:APPDATA 'dev.yukinal.workspace'
}
$db = Join-Path $dataDir 'knowledge\knowledge.sqlite'
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $db) | Out-Null

$cli = Join-Path (Split-Path -Parent $python) 'yukinal-knowledge.exe'
function Invoke-YukinalKnowledge {
  param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Arguments)
  if (Test-Path -LiteralPath $cli) {
    & $cli @Arguments
  } else {
    & $python -m yukinal_knowledge.cli @Arguments
  }
}

Invoke-YukinalKnowledge index --source $corpus --db $db
Invoke-YukinalKnowledge stats --db $db
Invoke-YukinalKnowledge doctor --db $db
Invoke-YukinalKnowledge search 'nginx 504 long request' `
  --db $db --mode hybrid --limit 5 --format json
```

`pip install -e "agent[agent]"` 应生成虚拟环境内的
`yukinal-knowledge.exe`。若 console script 未安装，上面的函数会自动回退到
`python -m yukinal_knowledge.cli`；也可直接使用这个模块入口。不要把
`python -m yukinal_knowledge.cli` 与已安装脚本混写成不存在的命令。

如果希望把数据库留在仓库的 `var\knowledge`，同时保持桌面可发现，可在启动
（或重启）桌面之前、在同一个 PowerShell 环境里设置绝对路径覆盖：

```powershell
$db = Join-Path (Resolve-Path '.').Path 'var\knowledge\knowledge.sqlite'
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $db) | Out-Null
$env:YUKINAL_KNOWLEDGE_DB = $db
```

桌面会优先使用 `YUKINAL_KNOWLEDGE_DB`；必须从继承了该变量的 PowerShell 启动
或重启桌面，否则 sidecar 看不到它。使用默认数据目录方案时不要设置这个覆盖
变量。仓库的 `var/knowledge/knowledge.sqlite` 本身不会被桌面自动发现。

`index` 和 `delete` 会写知识数据库；`index --dry-run` 只校验和切块，不创建
数据库。`doctor`、`stats`、`search`、`eval` 以只读方式打开已有数据库，不会
创建或迁移它；`eval --report` 另行原子写入独立 JSON 报告，不会写知识数据库。
文档超过 96 KiB、单文档超过 128 chunks 或单个 chunk 超过 1200 字节时会显式
失败，不会静默截断。

## Front matter

每篇文档以 TOML front matter 开头。以下字段必填：

```toml
+++
title = "文档标题"
source = "https://example.invalid/reference"
updated_at = "2026-11-25"
environment = "demo"
server_tags = ["srv_nginx01"]
acl = ["public"]
+++
```

- `source` 是公开资料或内部手册的来源，不写真实凭据、内网地址或个人信息。
- `updated_at` 是 ISO 日期；样例统一标记为 `2026-11-25`。
- `environment` 可写一个字符串或字符串数组；查询按其中一个值匹配。
- `server_tags` 为空数组表示该文档不限于某个 server，非空时只匹配列出的
  server 标签。
- `acl` 至少有一个值。它是**检索过滤标签**，不是认证、会话身份或 SSH 授权。
  CLI 的 `--principal` 由调用者自报，不能用于宣称访问控制已经成立。
- 其他 TOML 键会保存在文档元数据中；只有 `tags` 参与 `--tag` 过滤。

## 评估集

`eval/questions.jsonl` 每行是一个独立 JSON 对象：

```json
{"id":"nginx-502-timeout","domain":"nginx","question":"...","expected_doc_ids":["nginx/502-upstream-timeout.md"],"expected_section_ids":["upstream-timeout","root-causes"],"key_facts":["upstream timed out","error.log","proxy_read_timeout"],"filters":{"environment":"demo","server":"srv_nginx01","principal":"public"},"expect_no_answer":false,"forbidden_doc_ids":[]}
```

`filters` 中的 `environment`、`server` 和 `principal` 按 AND 解释。
`server` 使用桌面实际可生成的稳定 ID（本夹具为 `srv_nginx01` 等），匹配带
对应 `server_tags` 的文档；`server_tags` 为空的文档是全局文档，在指定 server
时仍然可见。这是检索元数据语义，不是权限继承。运行时 `SearchFilters` 的
`global_only=True` 只返回这些全局文档；CLI 没有 `--global-only` 参数，无
`--server` 表示不做 server 过滤。

`expected_section_ids` 使用严格的唯一绑定规则：

- 每个 section ID 必须出现在 `expected_doc_ids` 中某篇文档的标题锚点里；
- 同一题的同一个 section ID 只能属于一篇 expected 文档，跨文档歧义会报错；
- `expected_doc_ids` 中的每篇文档都必须至少被该题的一个 section 覆盖。

这保证 Section Recall 可以判断“section 是否从声明的文档中召回”，而不是只在
任意文档里看到一个同名标题。

`expect_no_answer: true` 的题不能同时声明 expected 文档或 section。检索返回
任何文档都记为 false positive；不返回文档才记为 abstain。当前两题：
`k8s-vpa-no-answer` 和 `docker-cross-server-isolation`。

`forbidden_doc_ids` 是评估断言，不是运行时 ACL。它检查指定文档是否出现在
top-k 中；命中会计入 `forbiddenHitCount`，且 `eval --threshold` 会直接失败。
当前两个探针是 `nginx-502-forbidden-acl-bait` 和
`docker-cross-server-isolation`。`aclLeakageCount` 只是 deprecated 兼容别名，
不是权限泄漏指标。

JSONL 使用 UTF-8、每题一行，文件末尾保留换行；空行不算题目。`key_facts` 仅
供人工检查答案是否被检索片段支持，不参与 Recall。

每题的 `question` 必须落在引擎的查询预算内：`64` 个唯一可检索 token。中文按
单字、双字和整段 CJK 片段展开，64 个唯一 token 大致相当于 70–80 个字符的中文
问句；超限的题目会让 `eval` 以 `invalid_input` 退出（退出码 2），而不是静默
截断。当前 27 题实测最大 64 个（`linux-disk-or-inode`）、其余 ≤63。
`agent/tests/test_knowledge_store.py` 有一个用例直接读取本文件，防止后续编辑
把题目重新推过预算。

## 运行评估

在完成上面的快速开始后，沿用同一 PowerShell 会话中的 `$db`、`$questions`
和 `Invoke-YukinalKnowledge` 包装器：

```powershell
$report = Join-Path (Split-Path -Parent $db) 'eval-report.json'
Invoke-YukinalKnowledge eval `
  --db $db --questions $questions `
  --modes lexical,vector,hybrid --k 5 --threshold 0.9 `
  --report $report
```

`--threshold` 要求每个模式的 Recall@k 不低于阈值，并要求
`forbiddenHitCount == 0`；任一条件不满足返回退出码 4。加 `--report
var/knowledge/eval-report.json` 可原子保存完整逐题结果；这是独立报告文件，
不是知识数据库写入。JSON 模式适合自动化。`search` 的 `--threshold` 则在
检索后过滤 `score >= threshold` 的结果；分数是 RRF 融合分加有界精确匹配
bonus（上限 1.0），不是校准过的距离或概率。仅在零结果且传入
`--require-hit` 或 `--threshold` 时，`search` 才返回退出码 4；不带这两个
参数的零命中搜索返回 0。

2026-11-25 在 Windows 上对当前夹具实测：

| 模式 | Recall@5 | Precision@5 | MRR@5 | nDCG@5 | Section Recall | Forbidden hits | No-answer |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| lexical | 0.980000 | 0.208000 | 0.920000 | 0.925475 | 0.860000 (45/52) | 0 | 2/2 |
| vector | 0.980000 | 0.208000 | 0.920000 | 0.931186 | 0.880000 (46/52) | 0 | 2/2 |
| hybrid | 0.980000 | 0.208000 | 0.940000 | 0.940237 | 0.880000 (46/52) | 0 | 2/2 |

指标定义：

- Recall@5 只统计 25 道计分题中唯一 expected 文档的覆盖率。
- Precision@5 的分母固定为 5，而不是实际返回文档数；只返回 2 篇相关文档时
  Precision@5 上限是 0.4。它用于横向比较，不应解读为“结果里有 20.8% 相关”。
- MRR@5 取第一个相关文档的倒数排名；nDCG@5 使用二元相关性。
- Section Recall 按题内绑定 section 统计，再对计分题取平均；分母是 52 个
  expected section 绑定。
- No-answer 列是 `noAnswerCorrect/noAnswerTotal`，即无文档返回的 abstain
  比例，不是模型置信度。

已知不足：

- 三种模式都没有在 `nginx-504-long-request` 的 top-5 中覆盖两篇 expected 文档，
  Recall@5 为 0.980000；这是当前唯一的文档级 miss。
- lexical 命中 45/52 个绑定 section，vector 和 hybrid 各 46/52。未全额命中的
  题目是 `nginx-502-upstream-timeout`、`nginx-504-long-request`、
  `docker-exit-137`、`systemd-restart-backoff`、`linux-disk-or-inode` 和
  `nginx-502-forbidden-acl-bait`；lexical 还额外漏掉
  `nginx-502-prompt-injection-query`。
- 两道 global 题的 Recall@5 和 Section Recall 在三种模式下均为 1.0。
- 这是小规模 curated 夹具上的检索评估，不是生产语料容量、最终答案忠实度或
  真实环境正确性的证明。

## 接入桌面 Agent

sidecar 按以下顺序寻找独立语料库：

1. 环境变量 `YUKINAL_KNOWLEDGE_DB` 指向的数据库；
2. `<data-dir>/knowledge/knowledge.sqlite`，且只在文件已经存在时启用。

语料库以只读方式打开。检索失败或数据库无效时，语料能力被关闭并记录警告，
显式保存的记忆仍可工作。运行时没有语料 principal，因此只返回 `public` 文档；
server 对话使用空 `server_tags` 的全局文档和匹配该 server tag 的文档。桌面
全局对话通过运行时 `SearchFilters(global_only=True)` 只读取全局文档；这仍是
检索范围，不是身份认证或授权提升。

桌面直接导入 Markdown/TXT 记忆不会携带 ACL、environment 或 server tags，也不会
自动进入这个独立语料库。需要这些元数据时，用 CLI 建立语料库并配置上述路径。

## 边界

- 语料库和会话记忆库是两个独立 SQLite 数据库，没有跨库事务。
- `vector` 模式是 768 维本地 feature hash，无法召回完全无词面重叠的语义近似。
- 检索把所有候选 chunk 读入 Python 后过滤和排序；大语料需要 FTS/ANN 等专用
  索引，当前实现不宣称生产规模。
- `acl`、`environment` 和 `server_tags` 是检索过滤标签；CLI `--principal` 由
  调用者自报，不构成认证、授权或权限提升。桌面 UI 与 SSH 副作用仍由宿主审批
  链决定。
- 查询预算是 64 个唯一 token。sidecar 把用户原始 prompt 直接交给
  `memory_context()`，超过预算时该轮不检索语料库，只保留显式记忆并标记
  `recent-low-confidence`；这是显式降级，不是静默截断。
- 当前只自动评估检索指标，没有自动评估最终答案的 groundedness/faithfulness。
  `key_facts` 需要人工或另行构建的答案评估流程。
- 本夹具没有 live SSH 验证；具体主机结论必须用只读 SSH 工具重新核对。

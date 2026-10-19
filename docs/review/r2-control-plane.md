# R2-B 控制平面深度安全审查

## 范围与方法

**范围**（审查阶段只读；修复落地见第七节，那里注明了唯一被改动的三个源文件与三个测试文件）：

- `services/control-plane/src/yukinal_control_plane/**`（审查时 `app.py` 556 行、`service.py` 1400 行、`channel.py` 273 行；修复后 564 / 1429 / 274 行，其余文件未动：`nodeauth.py` 177 行、`schemas.py` 313 行、`models.py` 288 行、`security.py` 54 行、`errors.py` 55 行、`db.py` 50 行、`config.py` 49 行、`actions.py` 224 行）
- `services/control-plane/migrations/versions/**`（5 个 revision + `env.py`）
- `services/control-plane/tests/**`（6 个 `test_*.py` + `support.py`）
- `services/control-plane/pyproject.toml`

**方法**：

1. **先读源码再下结论**：每条结论都给出 `file:line` 与可核对的代码片段；grep 只用于穷尽调用点（`require_tenant`、`disabled`、`assert_no_secret_fields`、SQL 字面量）。
2. **实测优先**：所有判定都在**真实 app + 真实 SQLite schema + 真实 Ed25519 签名**上跑过（复用 `tests/support.py` 的 `ControlPlaneTestCase`，即 FastAPI 实际服务的那份 app）。探针脚本位于 `services/control-plane/.tmp/`（gitignored，不进入仓库）：`r2_probe.py`、`r2_probe2.py` … `r2_probe24.py`，探针编号 P1–P115（编号连续覆盖到 P115，中间少数编号因早期脚本重写而空缺；部分编号带 `b`/`c` 后缀表示同一次运行里的子结果）。文中引用的 P 编号可直接在对应文件里定位（例如 P115 在 `r2_probe24.py`）。
3. **数字独立重算**：路由数、测试数、文件数均单独计数，不引用其他文档。
4. **置信度**：高 = 源码逐行确认 + 本地可复现；中 = 源码确认、需特定部署条件；低/无法判定 = 缺少证据，单列。

**基线复现**（本次运行，退出码 0）：

```powershell
cd D:\yukinal\services\control-plane
& .\.venv\Scripts\python.exe -m unittest discover -s tests -p "test_*.py"
# Ran 129 tests in 10.788s
# OK
```

测试数独立重算（AST 计数 `test_` 开头的方法）：`test_approvals.py` 23 + `test_approval_lifecycle.py` 23 + `test_control_plane.py` 20 + `test_node_auth.py` 16 + `test_node_channel.py` 9 + `test_policy.py` 38 = **129** ✓，与 `unittest` 报告一致。

路由数独立重算（遍历 `app.routes`）：共 37 条，其中 `/v1` 下 32 条 = **操作员 27 条 + 节点 5 条**，另有 5 条非 `/v1`（`/openapi.json`、`/docs`、`/docs/oauth2-redirect`、`/redoc` 为 GET/HEAD，`/healthz` 为 GET）。

---

## 总体结论

控制平面的**单点实现质量总体较高**：SQL 全部走绑定参数（无字符串拼接）；节点签名规范形在当前字段组合下无歧义（三轮独立搜索：1680 组 P27、72 组 P112、构造性对照 P115，0 碰撞）；nonce/审批/任务槽位的抢占都是 `UPDATE ... WHERE` 条件更新，并发下不会双花；审批指纹把 `(kind, nodeId, payload)` 绑进 SHA-256，调用方无法自带指纹；`extra="forbid"` 让未知字段（包括 `apiKey`）在入站即被拒；服务启动时 `require_token()` 失败即拒绝启动；WS 握手在 `accept()` 之前完成鉴权。

但**威胁模型的 7 条里有 2 条完全成立、5 条部分成立**，其中三条构成可复现的绕过链：

1. **节点签名不覆盖它自己声称的身份**（`nodeauth.py:121-127` 的规范串不含 node id），配合 `nodes.public_key` 无唯一约束（`models.py:59,68`），**一把私钥可对应多个节点身份**；而租户由被选中的 node 行走（`service.py:201-204`），于是换掉未签名的 `X-Yukinal-Node` 头即可把请求落到另一租户的 node 行上。实测 P98：同一份签名字节，头指向 A 租户节点 → 404，头改为 B 租户节点（同公钥）→ **200，并把 B 租户任务的 `status/outcome/result` 全部改写**。
2. **失败的请求不烧 nonce**：`claim_nonce` 只在校验通过后调用（`service.py:212`），且 nonce 插入位于请求事务内（`app.py:60-63`），路由层抛错即整体回滚 → 被捕获的请求在 300 s 窗口内可反复重放，且**在系统状态变化后会从「被拒」变成「成功」**。实测 P82：首次 `409 invalid_transition`，操作员把任务推进到 `running` 后重放同样字节 → **200 succeeded**；P83：签名错误 401 后，同一 nonce 用正确签名 → 200。
3. **派发门禁只覆盖 `queued→running` 一条边**（`service.py:1028`），且回调校验只看审批的 state/时效、**从不比对租户/node/指纹**（`service.py:830-838`）。实测 P87/P88：审批被撤销、甚至整个 dangerous 层被设为 `deny` 之后，把任务从 `unknown` 拉回 `running` 仍返回 **200**（对照：新建同类任务分别 409/403）；P90/P91/P92：任务记录的 `approval_id` 换成**另一租户**、另一节点、或另一 payload 的审批，派发照样 200。

这三条共同的根因是**「认证」与「授权对象」之间缺少绑定**：签名不绑定身份，nonce 不绑定失败，审批不绑定动作。

此外还有一组**边界收窄**问题：密钥字段扫描只作用于 task `payload`（`app.py:247` 是全局唯一调用点），审批 payload、`apiKeyRef`、节点上报的 `result` 都能持久化并回显凭据形状的值；`result` 无大小上限（200 KB 可存）；tool-policy 的 `kind` 无长度上限，300 字符能写进 `String(128)` 列（SQLite 不强制）；`/docs` 与 `/openapi.json` 无鉴权可读；SQLite 路径下 `PRAGMA foreign_keys=0`，外键与级联在运行期不生效。

最后，**多租户在这个控制平面里是「数据分区」而不是「授权边界」**：操作员侧只有一个全局共享 bearer token（`config.py:31`、`app.py:65-76`），路径里的 `tenant_id` 只是选择器，没有任何按租户的鉴权。这是设计属性而非缺陷，但它决定了「跨租户」在本系统里的真实含义，必须在结论里说清楚。

### 与 R1 结论的差异（两处需要更正）

R1-C（`docs/review/r1-python.md`）在「已排查」里给出的两条结论，经 R2 深查后需要更正：

- R1 第 809 行写「`transition_node_task` / `require_task_authorisation`（`service.py:842-855,1070-1083`）在状态迁移前重校验授权」。实际上该重校验只在 **`queued→running`** 一条边上调用（`service.py:1028`），`unknown→running` 不调用；且 `task_authorisation_problem` 的正文只查 state/时效，不比对租户/node/指纹。R2 的 P87/P88/P89 与 P90/P91/P92 分别证实了这两点，因此 R1 的表述**过于宽泛**（结论方向由「无绕过」修正为「`queued→running` 这一条边无绕过」）。
- R1 第 810 行写「5 个 revision 都实际跑过。未发现 schema 漂移」。实际上 `tests/test_control_plane.py:341-391` 只在**空表**上迁移；在含审批数据的库上 `a74d212eb82a` 会失败（R2 的 P60/P71），失败后遗留 `_alembic_tmp_approvals` 导致重试也失败，而启动检查只看表名（`app.py:42-52`）→ 一个 schema 落后于模型的库会「健康启动」。这是 R1 的广度审查未触及的数据条件。

R1 记录且 R2 确认仍成立的点：WS 无服务端心跳（R1 的 L-3）、`security.py` 的密钥字段扫描只在 task payload 上调用（R1 的 M-4，R2 的 F-1 给出更完整的调用点穷尽与实测矩阵）、审批消耗的 CAS（R1 第 808-809 行，R2 的 P7/P45 实测并发安全）。R2 **不把这三项当作新发现重复计数**。

---

## 一、端点 × 鉴权 × 租户约束矩阵

`require_tenant`（`service.py:102-106`）是唯一同时检查**存在性**与 `disabled` 的函数，其调用点（grep 穷尽）为：`register_node:124`、`list_nodes:157`、`get_tenant_policy:432`、`upsert_tenant_policy:469`、`delete_tenant_policy:530`、`list_tool_policies:548`、`upsert_tool_policy:562`、`delete_tool_policy:604`、`list_tasks:1005`、`create_model_config:1098`、`list_model_configs:1131`、`list_approvals:1284`、`list_audit:1304`（共 13 处）。其余路由不经过它。

### 操作员路由（27 条，`Authorization: Bearer`）

| # | 方法 路径 | 鉴权 | 租户来源 | `require_tenant` | 条目级归属校验 | 禁用租户仍可用 |
|---|---|---|---|---|---|---|
| 1 | `GET /v1/metrics` | bearer | —（全局） | ✗ | — | 是（跨全部租户聚合，含已禁用租户；无 `{tenant_id}`，不计入下方 10 条） |
| 2 | `GET /v1/tenants` | bearer | —（全局） | ✗ | — | — |
| 3 | `POST /v1/tenants` | bearer | body | ✗ | — | — |
| 4 | `GET /v1/tenants/{t}` | bearer | path | ✓ (`app.py:157`) | — | 否（404） |
| 5 | `GET /v1/tenants/{t}/nodes` | bearer | path | ✓ (`app.py:167`) | — | 否（404） |
| 6 | `POST /v1/tenants/{t}/nodes` | bearer | path | ✓ (`app.py:176`) | — | 否（404） |
| 7 | `GET .../nodes/{n}` | bearer | path | **✗** | `tenant_id` 相等 (`service.py:151`) | **是（200）** |
| 8 | `POST .../nodes/{n}/heartbeat` | bearer | path | **✗** | `tenant_id` 相等 (`service.py:287`) | **是（200）** |
| 9 | `GET .../nodes/{n}/recoverable` | bearer | path | **✗** | `tenant_id` 相等 (`service.py:320`) | **是（200）** |
| 10 | `GET .../tasks` | bearer | path | ✓ (`app.py:238`) | — | 否（404） |
| 11 | `POST .../tasks` | bearer | path | **✗** | `tenant_id` 相等 (`service.py:944`) | **是（201）** |
| 12 | `GET .../tasks/{id}` | bearer | path | **✗** | `task.tenant_id` 相等 (`service.py:989`) | **是（200）** |
| 13 | `POST .../tasks/{id}/status` | bearer | path | **✗** | 同上 (`service.py:1064`) | **是（200）** |
| 14 | `GET .../model-configs` | bearer | path | ✓ (`app.py:293`) | — | 否（404） |
| 15 | `POST .../model-configs` | bearer | path | ✓ (`service.py:1098`) | — | 否（404） |
| 16 | `GET .../policy` | bearer | path | ✓ (`service.py:432`) | — | 否（404） |
| 17 | `PUT .../policy` | bearer | path | ✓ (`service.py:469`) | — | 否（404） |
| 18 | `DELETE .../policy` | bearer | path | ✓ (`service.py:530`) | — | 否（404） |
| 19 | `GET .../tool-policies` | bearer | path | ✓ (`service.py:548`) | — | 否（404） |
| 20 | `PUT .../tool-policies/{kind}` | bearer | path | ✓ (`service.py:562`) | — | 否（404） |
| 21 | `DELETE .../tool-policies/{kind}` | bearer | path | ✓ (`service.py:604`) | — | 否（404） |
| 22 | `GET .../approvals` | bearer | path | ✓ (`service.py:1284`) | — | 否（404） |
| 23 | `POST .../approvals` | bearer | path | **✗** | `tenant_id` 相等 (`service.py:1164`) | **是（201）** |
| 24 | `POST .../approvals/{id}/decide` | bearer | path | **✗** | `approval.tenant_id` 相等 (`service.py:1211`) | **是（200）** |
| 25 | `POST .../approvals/{id}/revoke` | bearer | path | **✗** | 同上 (`service.py:1262`) | **是（200）** |
| 26 | `POST .../approvals/expire` | bearer | path | **✗✗** | **完全无校验** (`service.py:675-715` 的 `tenant_id` 只用于过滤) | **是（200，且对不存在/已禁用的租户都返回 200）** |
| 27 | `GET .../audit` | bearer | path | ✓ (`service.py:1304`) | — | 否（404） |

#### 「禁用租户仍可用」这一列是怎么算出来的（可复算）

**判据**：把某个租户行置 `disabled=1`，对 27 条操作员路由各发一次**在租户启用时必定成功**的请求（body 与路径都合法），记录状态码；`status < 400` 记为「可用」。为排除「我的请求体本身就不合法导致 404/422」这种测量假象，同一批 27 个请求会在一个**启用**租户上再跑一遍作为对照，要求 27/27 全部 2xx。

| 计入 / 排除 | 规则 | 涉及路由 |
|---|---|---|
| **计入** | 路径含 `{tenant_id}`，且 `disabled` 后仍 `status < 400` | 下方 10 条（#7/#8/#9/#11/#12/#13/#23/#24/#25/#26） |
| **排除（全局）** | 路径**不含** `{tenant_id}`，没有租户可「绕过」，禁用与否都是 2xx | #1 `GET /v1/metrics`、#2 `GET /v1/tenants`、#3 `POST /v1/tenants` |
| **排除（未触发）** | `require_tenant` 缺席但请求被挡成 404/422 的 | 无。穷举后没有一条属于此类：凡是在禁用后返回 404 的路由，都确实调用了 `require_tenant`（#4/#5/#6/#10/#14/#15/#16/#17/#18/#19/#20/#21/#22/#27） |
| **排除（测量假象）** | 状态码 ≥400 但**不是**因为租户禁用，而是请求体/参数本身不合法 | 无。第一版探针（`.tmp/r2_probe25.py`）曾出现 5 个此类假象（#6/#17/#23/#24/#25 因占位 body 返回 422），已由 P121 的启用租户对照剔除，最终判定一律基于 `r2_probe26.py` |

**计数规则明确回答三个易混点**：
- 表中 `#1` 那一行的「是」是**全局聚合**语义（`GET /v1/metrics` 统计所有租户，包括已禁用的），它没有 `{tenant_id}` 路径参数，因此**不计入**这 10 条。矩阵里「是」一共出现 11 次，减去 #1 这个全局项，就是 10 条租户路由 —— 这正是本报告口头数字与矩阵表面行数对不上的原因。
- 「`require_tenant` 缺席但条目级校验仍在」**算入**这 10 条：那些校验（`service.py:151/287/320/944/989/1064/1164/1211/1262`）只比较 `tenant_id`，**不查 `Tenant.disabled`**，所以已知 id 的条目在租户被禁用后仍然可读写 —— 与「完全无校验」的 #26 一起构成同一条缺口。判据是**实测行为**（禁用后仍 <400），不是「有没有调用 `require_tenant`」这个静态特征。
- `404` 一律计为「不可用」，不区分它是 `require_tenant` 抛的还是条目级校验抛的 —— 因为对调用方而言两者都是「该租户不可访问」。

**那 10 条（编号与路径）**：

| # | 方法 路径 | 为什么在禁用租户上仍可用（file:line） |
|---|---|---|
| 7 | `GET /v1/tenants/{t}/nodes/{n}` | `require_node` 只比 `tenant_id`（`service.py:149-153`） |
| 8 | `POST /v1/tenants/{t}/nodes/{n}/heartbeat` | 同上（`service.py:287` 调用点） |
| 9 | `GET /v1/tenants/{t}/nodes/{n}/recoverable` | 同上（`service.py:320`） |
| 11 | `POST /v1/tenants/{t}/tasks` | `create_task` 只比 `node.tenant_id`（`service.py:944`） |
| 12 | `GET /v1/tenants/{t}/tasks/{id}` | `require_task` 只比 `task.tenant_id`（`service.py:987-991`） |
| 13 | `POST /v1/tenants/{t}/tasks/{id}/status` | 同上（`service.py:1064`） |
| 23 | `POST /v1/tenants/{t}/approvals` | `create_approval` 只比 `tenant_id`（`service.py:1164`） |
| 24 | `POST /v1/tenants/{t}/approvals/{id}/decide` | 只比 `approval.tenant_id`（`service.py:1211`） |
| 25 | `POST /v1/tenants/{t}/approvals/{id}/revoke` | 同上（`service.py:1262`） |
| 26 | `POST /v1/tenants/{t}/approvals/expire` | **链路上完全没有租户校验**，`tenant_id` 只作为 `WHERE` 过滤条件（`service.py:675-715`），且对**不存在**的租户也返回 200 |

**实测（`.tmp/r2_probe26.py`，`#3` 是空 body 的全局创建路由，不计入）**：
```
P118  disabled tenant ten_dis
  served (status<400)          : #1 #2 #3 #7 #8 #9 #11 #12 #13 #23 #24 #25 #26   (13 条)
  blocked by require_tenant    : #4 #5 #6 #10 #14 #15 #16 #17 #18 #19 #20 #21 #22 #27  (14 条，全部 404 code=not_found)
  减掉全局 #1/#2/#3 → 租户路径上仍可用 = 10 条：#7 #8 #9 #11 #12 #13 #23 #24 #25 #26
P121  同一批 27 个请求打到「启用」租户 → 27/27 全部 2xx（证明 404 只可能来自 disabled，不是 body 非法）
```

#### #26 对**不存在**的租户返回 200（最小复现，非推断）

请求（`Authorization: Bearer` 为控制平面配置的共享 token；无 token 或错误 token 都先被 `current_actor` 挡成 401）：

```http
POST /v1/tenants/ten_nope/approvals/expire HTTP/1.1
Authorization: Bearer <YUKINAL_CONTROL_TOKEN>
```

响应（实测 P119）：

```
200 {"expired":0}

对照同一租户名的其他路由（全部 404，且 code=not_found）：
  GET /v1/tenants/ten_nope               -> 404 {"code":"not_found","message":"tenant `ten_nope` was not found"}
  GET /v1/tenants/ten_nope/nodes          -> 404 同上
  GET /v1/tenants/ten_nope/tasks          -> 404 同上
  GET /v1/tenants/ten_nope/approvals      -> 404 同上
  GET /v1/tenants/ten_nope/audit          -> 404 同上
  GET /v1/tenants/ten_nope/policy         -> 404 同上
```

鉴权仍然生效（P120）：无 `Authorization` → `401 {"detail":"unauthorized"}`；错误 token → `401`；正确 token → `200 {"expired":0}`。**所以这不是「匿名可达」，而是「持 token 者对任意租户名都能执行一个会改状态的操作」。**

**更正一处措辞**：把 200 说成「不存在的租户被确认为可操作」并不准确——实测 P123 显示 `ten_live`（启用）、`ten_off`（已禁用）、`ten_ghost`（从未存在）三者的 `expire` 响应**完全一致**（都是 `200 {"expired":0}`），所以它**不是存在性预言机**。准确表述是：**#26 是一个无租户门禁的状态变更操作**——对已禁用租户它会真的改数据（P122：`{"expired":1}`，把 `approved` 改成 `expired`，并向该租户写入一条 `approval.expired` 审计），而对不存在的租户只是空转返回 200。这比「确认存在」更严重，因为它同时说明「禁用开关挡不住写入」。

### 节点路由（5 条，Ed25519 签名）

| # | 方法 路径 | 鉴权 | 租户来源 | 禁用租户 | 归属校验 |
|---|---|---|---|---|---|
| 1 | `POST /v1/node/heartbeat` | 签名 (`app.py:499`) | node 行 (`service.py:201-204`) | **拒绝** (`service.py:203`) | node 自身 |
| 2 | `GET /v1/node/tasks` | 签名 (`app.py:517`) | node 行 | **拒绝** (`service.py:177`) | `node_id` 相等 (`service.py:342`) |
| 3 | `GET /v1/node/recoverable` | 签名 (`app.py:525`) | node 行 | **拒绝** | `node_id` 相等 (`service.py:329`) |
| 4 | `POST /v1/node/tasks/{id}/status` | 签名 (`app.py:533`) | node 行 | **拒绝** | `task.node_id` 相等 (`service.py:997`) |
| 5 | `WS /v1/node/channel` | 握手签名，`accept()` 前 (`channel.py:110-115`) | node 行 | **拒绝** | 同上 |

**结论**：节点侧对 `disabled` 的处理**严于**操作员侧；但节点侧的身份绑定存在缺口（见第二节）。操作员侧 27 条路由中，**10 条含 `{tenant_id}` 的路由在租户被禁用后仍然可用**（#7、#8、#9、#11、#12、#13、#23、#24、#25、#26；#1 `GET /v1/metrics` 也是 2xx，但它是无租户参数的全局聚合，不计入这 10 条）。计数规则与逐条依据见上一小节，实测在 `.tmp/r2_probe26.py`。

---

## 二、逐项判定

### 项 1：多租户隔离 —— **部分成立**

**1a. 操作员侧没有按租户的授权，只有全局共享 token（设计属性）**

```python
# app.py:65-76
    async def current_actor(
        request: Request,
        authorization: str | None = Header(default=None),
        x_yukinal_actor: str | None = Header(default=None, alias="X-Yukinal-Actor"),
    ) -> str:
        token = request.app.state.settings.require_token()
        if token is not None:
            expected = f"Bearer {token}"
            if authorization is None or not _constant_time_equal(authorization, expected):
                raise HTTPException(status_code=401, detail="unauthorized")
        actor = (x_yukinal_actor or "operator").strip()
        return actor or "operator"
```

`tenant_id` 只从路径取，任何 token 持有者对任何 `tenant_id` 都通过。因此**「操作员能否读别的租户」这个问法在本系统里不构成边界**——设计上就是一个全局管理员 token。风险表述应改为：token 一旦泄露，影响面是**全部租户**，且审计里的 `actor` 完全由调用方自填（见附加发现 F-4），无法事后区分是谁做的。

**1b. 禁用租户的「kill switch」只挡住列表类路由**

实测（P76）：把租户置为 `disabled=1` 后，

```
create_approval   -> 201   （审批被创建）
create_task       -> 201   （任务被创建）
decide_approval   -> 200   （审批被批准）
operator heartbeat-> 200   （心跳成功，可把 running 任务打成 unknown）
task status       -> 200   （任务被推进到 running）
list_tasks        -> 404
get_tenant        -> 404
node 签名 /v1/node/tasks -> 401  （节点侧确实挡住了）
```

根因：`require_node`（`service.py:149-153`）只比较 `node.tenant_id == tenant_id`，不查 `Tenant.disabled`；`require_task`（`service.py:987-991`）只比较 `task.tenant_id`。而 `require_tenant` 才检查 `disabled`。

```python
# service.py:149-153
async def require_node(session: AsyncSession, tenant_id: str, node_id: str) -> models.Node:
    node = await session.get(models.Node, node_id)
    if node is None or node.tenant_id != tenant_id:
        raise NotFound(f"node `{node_id}` was not found")
    return node
```

**影响**：`disabled` 的语义在两个方向上不一致——列表看不见，但已知 id 的条目仍可操作。若 `disabled` 被用作「停止该租户一切活动」的开关，则它**不成立**；若只是「从列表隐藏」，则文档语义需要改。

穷举后的精确范围见第一节的「怎么算出来的」小节：**10 条**含 `{tenant_id}` 的路由在禁用后仍 <400（#7/#8/#9/#11/#12/#13/#23/#24/#25/#26），另有 14 条被 `require_tenant` 挡成 404。其中 #26 最严重——它不只是「绕过禁用」，而是链路上**完全没有**租户校验（`service.py:675-715`），实测对已禁用租户会真的改数据并写审计（P122），对不存在的租户也返回 200（P119）。

**1c. 跨租户读/写路径（成立，有前置条件）**

节点身份 = 未签名的 `X-Yukinal-Node` 头 + 用该 node 行公钥验签。`canonical_request` 不含 node id：

```python
# nodeauth.py:121-127
def canonical_request(
    *, timestamp: str, nonce: str, method: str, path: str, body: bytes
) -> bytes:
    """The exact bytes a node signs and the control plane verifies."""
    return "\n".join(
        [timestamp, nonce, method.upper(), path, body_digest(body)]
    ).encode("utf-8")
```

验签时按 `node_id` 取行、再用**那一行**的公钥验签：

```python
# service.py:199-213
    if not (node_id and timestamp and nonce and signature):
        raise Unauthorized("node authentication headers are missing")
    node = await session.get(models.Node, node_id)
    tenant = await session.get(models.Tenant, node.tenant_id) if node is not None else None
    if node is None or tenant is None or tenant.disabled:
        raise Unauthorized("node authentication failed")
    ...
    if not nodeauth.verify_signature(node.public_key, signature, canonical):
        raise Unauthorized("the request signature is not valid for this node")
    await claim_nonce(session, node_id=node.id, nonce=nonce)
    return node
```

`nodes` 上只有 `(tenant_id, name)` 唯一，**`public_key` 没有唯一约束**：

```python
# models.py:57-68
class Node(Base):
    __tablename__ = "nodes"
    __table_args__ = (UniqueConstraint("tenant_id", "name", name="uq_nodes_tenant_name"),)
    ...
    public_key: Mapped[str] = mapped_column(String(2048), nullable=False)
```

注册接口不禁止复用公钥（实测 P56：同一把 Ed25519 公钥，同租户第二次注册 → 201，跨租户注册 → 201）。于是**同一把密钥对应多个 node 行**，而「我是谁」由未签名的头决定。

**最小复现**（写入 `services/control-plane/.tmp/repro_nodswap.py` 后运行）：

```python
import sys; sys.path[:0] = [r"D:\yukinal\services\control-plane\tests", r"D:\yukinal\services\control-plane\src"]
import unittest
from support import AUTH, ControlPlaneTestCase, body_bytes, new_key
from yukinal_control_plane import nodeauth

class Repro(ControlPlaneTestCase):
    def test_cross_tenant_write(self):
        key = new_key()
        a, b = self.create_tenant("A"), self.create_tenant("B")
        node_a = self.register_node(a, key, "a-01").json()["id"]
        node_b = self.register_node(b, key, "b-01").json()["id"]
        task_b = self.create_task(b, node_b, "filesystem.read", {"path": "/B/x"})["body"]["id"]

        path = f"/v1/node/tasks/{task_b}/status"
        body = body_bytes({"status": "running", "outcome": "written-by-a"})
        headers = self.node_headers(key, "POST", path, body, node_id=node_a)

        # 同一次签名（同 nonce、同字节）：头指向 A 租户 -> 404
        r1 = self.client.post(path, content=body, headers={**headers, "Content-Type": "application/json"})
        # 只改未签名的 node 头为 B 租户节点 -> 200，并改写 B 的任务
        r2 = self.client.post(path, content=body,
                              headers={**headers, nodeauth.HEADER_NODE: node_b, "Content-Type": "application/json"})
        print(r1.status_code, r2.status_code, self.get_task(b, task_b)["status"])

unittest.main()
```

实测输出（P98）：

```
header_node_a=[404, "task `tsk_ad4c877c5f8238c7` was not found"]
header_node_b=[200, "running"]
task_b_status="running"  task_b_outcome="written-by-a"  task_b_result={"stolen": true}
```

同一条缺口在 WS 侧同样成立（P99）：同一握手签名，头改为 B 租户节点后，`ready` 帧回的是 **B 租户节点的 nodeId 与 `assigned: 1`**，B 租户的任务 payload 被推送给了这次连接。

还可跨租户读取（P66）：同一份字节，头指向另一租户节点 → `200`，返回 `[{"path": "/B/TOP"}]`。

**前置条件（必须写明）**：需要持有那把私钥，且该公钥在目标租户下也存在一行节点。注册需要操作员 token。因此这条路径的现实形态是：**同一把密钥被登记进两个租户（配置错误、证书复用、批量 provisioning 复用密钥），其中一个租户的节点即可操作另一个租户的数据**，且不需要该租户的任何凭据。同理，同租户内一把密钥的两个 node 行完全等价（P66：头指向 `a-02` 时看不到 `a-01` 的任务，但可以代替 `a-01` 接收派发）。

**判定**：跨租户路径 **成立（条件性）**；根因是「签名不绑定身份 + 公钥可复用」。

---

### 项 2：节点 Ed25519 签名规范化歧义与重放 —— **部分成立**（规范化不成立，重放成立）

**2a. 规范化无歧义 —— 不成立（无缺陷），但存在已被测量关闭的结构性弱点**

规范串是 5 个字段以 `\n` 连接，第 5 个是 body 的 SHA-256 十六进制：

```python
# nodeauth.py:121-127
def canonical_request(
    *, timestamp: str, nonce: str, method: str, path: str, body: bytes
) -> bytes:
    """The exact bytes a node signs and the control plane verifies."""
    return "\n".join(
        [timestamp, nonce, method.upper(), path, body_digest(body)]
    ).encode("utf-8")
```

把用户可控字段用 `\n` 连接，在结构上允许**字段边界平移**（若某字段含 `\n`，就能与相邻字段互换内容）。这类编码歧义是否可真利用，取决于「是否存在两个可同时含 `\n` 的相邻字段」。逐项测量：

| 字段 | 能否含 `\n` | 证据 |
|---|---|---|
| `timestamp` | 否 | `timestamp_within_window` 要求 `int(timestamp)` 成功（`nodeauth.py:133-136`） |
| `nonce` | **能** | P106：`X-Test: "a\nb"` 原样穿过 HTTP 层（`repr='a\nb'`, len=3）；P107：用含 `\n` 的 nonce 签名 → **200**，服务端用同一含 `\n` 的 nonce 重建规范串并通过验签 |
| `method` | 否 | 由 `request.method` 提供，HTTP 方法名不含控制字符 |
| `path` / `query` | 否 | P113：`/a%0Ab`、`/a%0A/b`、`/a%250Ab` 全部 **404**（未匹配到任何路由，说明 `%0A` 没有被解码进 `request.url.path`）；字面 `\n` 直接被 URL 解析器拒绝（`InvalidURL: Invalid non-printable ASCII character in URL`）。`request.url.query` 取的是**未解码**的原始查询串，`%0A` 保持字面文本 |
| `body_digest` | 否 | 定长 64 位十六进制 |

因为唯一能含 `\n` 的字段是 `nonce`，而平移边界需要**另一个**可含 `\n` 的相邻字段（`path`），所以构造不出两条不同请求共享同一规范串：

- P112（边界平移穷尽搜索）：72 组 `(nonce, method, path, digest)` 组合 → 72 个唯一规范串，**0 碰撞**。
- P115（构造性对照）：`(nonce="a", path="/v1/node/tasks")` 与 `(nonce="a\nGET", path=…)` 产生的串长度/字段数不同，**不相等**。
- P108/P110/P111：试图把签名从 `/v1/node/tasks` 平移给 `/v1/node/recoverable`（或任意伪造路径）→ 规范串不相等 → **401**；伪造路径本身 **404**。
- P27（早期版本，1680 组含路径注入的搜索）→ 无碰撞。

**结论**：当前字段组合下不可利用；但这依赖「`path` 恰好不能含 `\n`」这一**外部实现约束**（取决于 ASGI 服务器/URL 解析器的行为），而不是协议自身的约束。建议在规范串里对每个字段加长度前缀（如 `len:value`）或改用结构化编码，使歧义在协议层不可能——这是防御性加固，不是当前缺陷。

一处**规范化与实现的细微差异**值得记录（不构成漏洞）：`verify_signature` 用 `base64.b64decode(..., validate=True)`，而 `sign_headers` 用标准 base64 输出，两侧一致；`parse_public_key`（`nodeauth.py:47-89`）同时接受 base64 与 hex，注册时被 `schemas.py:60-72` 归一到 base64。

**2b. 重放 —— 成立**

三层问题叠加：

1. `claim_nonce` 在验签**之后**才调用（`service.py:212`），所以**签名错误 / 时间戳越界 / 节点不存在**时 nonce 根本不写入——这是正常的，但意味着攻击者可以对同一 nonce 无限次尝试签名（无速率限制，见项 4）。
2. nonce 的插入发生在**请求事务内**（`app.py:60-63` 的 `async with session.begin()`），而路由层抛出的 `ControlPlaneError`（404/409/403）会让整个依赖生成器回滚 → **被拒的请求不烧 nonce**。
3. 因此一条被捕获的合法请求在 `SIGNATURE_WINDOW_SECONDS = 300`（`nodeauth.py:130-138` 的 `abs(reference - value) <= window`）窗口内可反复重放，且**系统状态变化后同一字节会从「被拒」变为「成功」**。

```python
# service.py:216-232
async def claim_nonce(
    session: AsyncSession, *, node_id: str, nonce: str, now: datetime | None = None
) -> None:
    """Consume a request nonce, raising ``Unauthorized`` if it was already used."""
    stamp = now or utc_now()
    session.add(models.NodeNonce(node_id=node_id, nonce=nonce, at=stamp))
    try:
        await session.flush()
    except IntegrityError as error:
        raise Unauthorized("this request nonce has already been used") from error
    # Keep the table proportional to the signature window instead of growing.
    await session.execute(
        delete(models.NodeNonce).where(
            models.NodeNonce.at
            < stamp - timedelta(seconds=nodeauth.SIGNATURE_WINDOW_SECONDS * 2)
        )
    )
```

**最小复现**（`services/control-plane/.tmp/repro_replay.py`）：

```python
import sys; sys.path[:0] = [r"D:\yukinal\services\control-plane\tests", r"D:\yukinal\services\control-plane\src"]
import unittest
from support import ControlPlaneTestCase, body_bytes

class Repro(ControlPlaneTestCase):
    def test_failed_request_does_not_burn_the_nonce(self):
        t = self.create_tenant()
        node, key = self.provision_node(t)
        task = self.create_task(t, node, "filesystem.read", {"path": "/x"})["body"]["id"]
        path = f"/v1/node/tasks/{task}/status"
        body = body_bytes({"status": "succeeded", "outcome": "replayed"})
        headers = {**self.node_headers(key, "POST", path, body, node_id=node),
                   "Content-Type": "application/json"}

        first = self.client.post(path, content=body, headers=headers)    # queued -> succeeded 非法
        self.move(t, task, "running")                                   # 操作员推进状态
        third = self.client.post(path, content=body, headers=headers)    # 同一字节、同一 nonce
        print(first.status_code, third.status_code, self.get_task(t, task)["outcome"])

unittest.main()
```

实测输出（P82）：

```
first=[409, "invalid_transition"]   immediate_replay=[409, "invalid_transition"]
replay_after_operator_move=[200, "succeeded"]
```

以及 P83（签名错误不烧 nonce）：

```
bad_signature=401   same_nonce_correct=200
```

对照：**成功的请求会烧掉 nonce**（P4：成功后重放 → `401 this request nonce has already been used`；成功后 `node_nonces` 中该 nonce 保留 1 行）。

**2c. nonce 无长度上限 —— 部分成立**

`node_nonces.nonce` 声明为 `String(128)`（`models.py:104`），但请求侧没有任何长度校验（`service.py:199` 只检查非空）。实测 P68：129 / 1000 / 100000 字符的 nonce 全部 **200 并通过**，SQLite 中按 129 / 1000 / 100000 字符原样存储（SQLite 不强制 `VARCHAR` 长度）。

```python
# models.py:101-107
    node_id: Mapped[str] = mapped_column(
        ForeignKey("nodes.id", ondelete="CASCADE"), primary_key=True
    )
    nonce: Mapped[str] = mapped_column(String(128), primary_key=True)
```

**影响**：`node_nonces` 的大小是有界的——只有**验签通过**的请求才会真的写入行，且同一次调用里的 `DELETE`（`service.py:227-232`，阈值 600 s）会把超过窗口的行清掉；失败请求会把 INSERT 与 DELETE 一起回滚（P16：失败请求后 `node_nonces` 行数为 0）。所以真正的缺口不是「表会无界增长」，而是**每一行可以任意长**：SQLite 不强制 `String(128)`，100000 字符的 nonce 原样入库（P68），使表容量 = 请求数 × 非预期行长。在 PostgreSQL 上 `varchar(128)` 会强制长度，超长时的行为（干净的 4xx 还是 `asyncpg` 驱动错误 → 500）**无法判定**（U-2）。同类问题还有 `X-Yukinal-Actor`（声明 `String(200)`，实测 500 字符原样入库，P57）。

**2d. 时钟偏移 —— 不成立（无缺陷）**

窗口是双向的 `abs(reference - value) <= 300`，不存在「只接受过去」的偏向；`timestamp_within_window` 对非整数返回 False（fail-closed）。节点与服务器时钟不同步超过 300 s 会整体拒绝，这是可接受的设计取舍。

---

### 项 3：审批绑定与策略绕过 —— **成立**

**3a. 创建侧绑定是正确的 —— 不成立（无缺陷）**

`available_approval` 把租户、动作、指纹、时效、剩余次数全部放进 `WHERE`：

```python
# service.py:727-744
    fingerprint = actions.action_fingerprint(kind=kind, node_id=node_id, payload=payload)
    now = utc_now()
    return await session.scalar(
        select(models.Approval)
        .where(
            models.Approval.tenant_id == tenant_id,
            models.Approval.action == kind,
            models.Approval.fingerprint == fingerprint,
            models.Approval.state == "approved",
            models.Approval.uses < models.Approval.max_uses,
            or_(
                models.Approval.expires_at.is_(None),
                models.Approval.expires_at > now,
            ),
        )
```

`consume_approval` 用条件更新做原子消耗（`service.py:747-769`），实测并发安全（P7/P45：两个/三个并发消耗一个单次审批 → 结果集合为 `["True","False"]`（顺序不定），最终 `uses=1/max_uses=1`，无超卖）。指纹把 node id 纳入（`actions.py:208-218`），所以「同一命令在两个节点上是两个动作」成立，单测也覆盖了跨节点/跨租户/跨 payload 的不匹配（`tests/test_approvals.py:107-156`）。

**3b. 派发侧不重验绑定 —— 成立**

```python
# service.py:830-838
    if task.approval_id is None:
        return "approval_required"
    approval = await session.get(models.Approval, task.approval_id)
    if approval is None:
        return "approval_missing"
    if approval.state != "approved":
        return f"approval_{approval.state}"
    if not _decision_is_live(approval, utc_now()):
        return "approval_expired"
    return None
```

只查 state 与时效。**没有**任何一处比对 `approval.tenant_id == task.tenant_id`、`approval.node_id == task.node_id`、或 `approval.fingerprint == task.fingerprint`。文档字符串说「Dispatch re-checks the decision the task recorded」，但「它记录的」实际只被读成一个外键值。

由于 `tasks.approval_id` 是刻意的**软引用**（`models.py:133-138`，无 FK），任何能写该列或让该列与审批不匹配的路径都会绕过：

**最小复现 A：跨租户 / 跨 node / 跨 payload 的审批都能解锁派发**

```python
import sys, sqlite3; sys.path[:0] = [r"D:\yukinal\services\control-plane\tests", r"D:\yukinal\services\control-plane\src"]
import unittest
from support import ControlPlaneTestCase

class Repro(ControlPlaneTestCase):
    def test_cross_tenant_approval_unlocks_dispatch(self):
        a, b = self.create_tenant("A"), self.create_tenant("B")
        node_a, _ = self.provision_node(a, "a-01")
        node_b, _ = self.provision_node(b, "b-01")
        approval_b = self.approve(b, node_b, "server.exec", {"command": "id"})  # B 租户的审批
        self.approve(a, node_a, "server.exec", {"command": "id"})               # A 自己的审批
        task = self.create_task(a, node_a, "server.exec", {"command": "id"})["body"]["id"]

        con = sqlite3.connect(self.db_path)
        con.execute("UPDATE tasks SET approval_id = ? WHERE id = ?", (approval_b, task)); con.commit(); con.close()
        print(self.move(a, task, "running"), self.get_task(a, task)["status"])

unittest.main()
```

实测输出：P90 `dispatch_http=200 status_after_dispatch="running"`（审批属租户 B、节点 B，任务属租户 A、节点 A）；P91 同租户跨节点 `200 running`；P92 审批 payload 为 `{"command":"id"}`、任务 payload 为 `{"command":"rm -rf /"}`，`200 running`。

**必须诚实标注的一点**：这三条的最小复现里，`approval_id` 是用 `UPDATE` 直写的（因为 API 没有改写它的入口）。因此它们证明的是「**派发门禁的校验不完整**」这一**代码事实**（`service.py:830-838` 确实不比对任何绑定字段），而**不是**「存在一条纯 HTTP 的改写路径」。任务创建时 `approval_id` 由 `available_approval` 正确绑定（`service.py:948-962`），所以纯 HTTP 路径下 A 租户无法把自己的任务挂到 B 的审批上——实测印证：A 租户无自己的审批时创建任务被拒（P90 `create_without_own_approval=[409,"approval_required"]`）。这一条的严重性取决于「谁能写 `tasks` 表」：数据库直连、运维脚本、未来新增的写路径都会立刻变成可利用的审批绕过。**建议按「防御纵深缺失」处理，而不是按「当前可利用」处理**——但这不改变修复的紧迫性，因为修复成本极低（三个字段比对）。

**3c. `unknown→running` 不重验授权 —— 成立（纯 HTTP 可利用）**

```python
# service.py:1027-1033
    previous = task.status
    if status == "running" and previous == "queued":
        # Re-checked here as well as at creation: dispatch is the moment the node
        # is told to act, so the gate belongs on this edge too. The check follows
        # the decision the task recorded, which catches one that lapsed or was
        # withdrawn in between.
        await require_task_authorisation(session, task)
```

状态机允许 `unknown → running`（`_TRANSITIONS`：`unknown→{running,succeeded,failed,cancelled}`），而 `unknown` 是**由节点可控事件产生的**：节点心跳 `restarted=true`（`service.py:293-296`）或 WS 掉线/`close`（`service.py:301-315`、`channel.py:248-255`）都会把 `running` 任务打成 `unknown`。于是：

```
节点/操作员：queued -> running     （过门禁，合法）
节点上报 restarted / 断线          -> 任务变 unknown
租户撤销审批 / 把 dangerous 设为 deny
（节点或操作员）unknown -> running  -> 200，且 attempts += 1，门禁完全未跑
```

**最小复现（纯 HTTP，无直写数据库）**：

```python
import sys; sys.path[:0] = [r"D:\yukinal\services\control-plane\tests", r"D:\yukinal\services\control-plane\src"]
import unittest
from support import AUTH, ControlPlaneTestCase

class Repro(ControlPlaneTestCase):
    def test_revoked_approval_cannot_stop_a_reasserted_task(self):
        t = self.create_tenant()
        node, _ = self.provision_node(t)
        approval = self.approve(t, node, "server.exec", {"command": "id"})
        task = self.create_task(t, node, "server.exec", {"command": "id"})["body"]["id"]

        assert self.move(t, task, "running") == 200
        self.revoke(t, approval)                                     # 撤销授权
        self.client.post(f"/v1/tenants/{t}/nodes/{node}/heartbeat",
                         json={"restarted": True}, headers=AUTH)     # -> unknown
        print("reassert:", self.move(t, task, "running"),            # 200（应当被拒）
              "status:", self.get_task(t, task)["status"],
              "fresh:", self.create_task(t, node, "server.exec", {"command": "id"})["status"])  # 409

unittest.main()
```

实测输出（P87）：

```
queued_to_running=200  revoke=200  marked_unknown=["tsk_..."]
status_after_restart="unknown"
unknown_to_running_http=200  status_after_reassert="running"  attempts=2
fresh_task_with_revoked_approval=[409, "approval_required"]
```

把 `dangerous` 层整层设为 `deny` 后同一条路径同样 200（P88 `unknown_to_running_http=200`，对照 `fresh_task_under_deny=[403,"action_denied"]`）；节点用自己的 `/v1/node/tasks/{id}/status` 也能把 `unknown` 拉回 `running`（P89：`node_reassert_running=[200,"running"]`）。

**影响**：`deny` 被文档描述为「A denial is absolute; no decision can clear it」（`tests/test_policy.py:419-430` 明确了这条不变量，单测也覆盖了 `deny` 阻挡 `queued→running`）。但 `unknown→running` 这条边让**已被绝对拒绝的动作重新拿到一次执行机会**，节点侧自己就能触发（先断线把任务打成 unknown，再上报 running）。这直接违反代码自身声明的不变量。

**判定**：项 3 **成立**（3b 为防御纵深缺失，3c 为纯 HTTP 可利用的授权绕过）。

---

### 项 4：WebSocket 节点通道 —— **部分成立**

**4a. 握手鉴权在 accept 之前 —— 不成立（无缺陷）**

```python
# channel.py:110-117
    try:
        node_id = await _authenticate(websocket, session_factory)
    except Unauthorized as error:
        # Refuse before accepting: an unauthenticated peer never gets a socket.
        await websocket.close(code=CLOSE_UNAUTHORIZED, reason=str(error))
        return

    await websocket.accept()
```

实测（P95）：未知节点 / 错误密钥都得到 `[4401, "node authentication failed"]` / `[4401, "the request signature is not valid for this node"]`——**未鉴权的对端确实拿不到 socket**。但 close reason 泄露了具体原因（见项 6）。

**4b. 消息 schema 校验 —— 部分成立**

`_pump` 手工校验：

```python
# channel.py:158-170
        raw = await websocket.receive_text()
        if len(raw.encode("utf-8")) > MAX_MESSAGE_BYTES:
            await websocket.send_json(_error("the message is too large", code="message_too_large"))
            continue
        try:
            message = json.loads(raw)
        except json.JSONDecodeError:
            await websocket.send_json(_error("the message is not valid JSON", code="invalid_json"))
            continue
        if not isinstance(message, dict):
            await websocket.send_json(_error("the message must be a JSON object"))
            continue
```

- 未知 type → `unknown_message`（`channel.py:184-187`），实测 P9/P43 通过。
- 超大帧 → `message_too_large`，且**后续消息仍能正常处理**（P42：超限回复之后 `pong` 仍被服务），连接未被误杀。
- `status` 的字段按类型手工过滤（`channel.py:219-239`）：`taskId`/`status` 必须 `str`，`outcome` 仅接受 `str`，`result` 仅接受 `dict`。这与 HTTP 侧的 pydantic 校验**不等价，且宽松得多**：`TaskStatusUpdate.outcome` 有 `max_length=64`（`schemas.py:111`），WS 侧无任何长度约束。实测 **P102**：

  ```
  同一条 5000 字符的 outcome：
    HTTP POST /v1/tenants/{t}/tasks/{id}/status  -> 422（schema max_length=64）
    WS   {"type":"status",...,"outcome":"o"*5000} -> ack，落库 outcome 长度 = 5000
  WS 同帧携带 40 KB result -> 落库 result 长度 = 40012
  ```

  即 **WS 是绕过 HTTP schema 上限的一条通道**：`outcome` 落进声明为 `String(64)` 的列（`models.py:143`），`result` 落进无约束的 JSON 列（`models.py:144`）。单帧仍受 64 KB 应用上限（以及 16 MiB 服务器上限）约束，因此放大倍数有限，但「同一份状态更新走 HTTP 被拒、走 WS 通过」是明确的校验不一致。
- **大小检查发生在 `receive_text()` 之后**（P78：源码中 `receive_text` 的位置早于 `MAX_MESSAGE_BYTES` 检查），也就是说 64 KB 的应用上限是对**已缓冲**的帧生效；缓冲上限实际由服务器决定。实测 uvicorn 的 `ws_max_size` 默认 **16 MiB**（P77），`Config.ws_max_size=16777216`。因此单连接的内存下界是 16 MiB 而非 64 KiB。注释写的是「refused rather than buffered」（`channel.py:51`），与实现不符。
- `TaskView` 载荷通过 `_task_payload` 用 pydantic 序列化（`channel.py:67-68`），工人侧结构是收敛的。

**4c. 背压/泛洪 —— 部分成立**

- 无服务端 ping 定时器（代码注释自述：`channel.py:31-32`「there is no server-side heartbeat timer yet」），静默连接只能靠 socket 断开发现。**这一点 R1 已记录过**（`docs/review/r1-python.md` 的 L-3），此处只作为「背压/洪泛」判定的一部分复述，**不计为 R2 新发现**。
- 无速率限制：实测 P9/P43 **200 次 `sync` 往返全部即时返回**，无节流。每次 `sync` 都会开事务、`touch_node`、扫 queued 任务（`channel.py:197-210`），节点可凭一纸合法密钥持续制造数据库负载。这是 R2 的新发现（R1 未覆盖速率维度）。
- 单节点只能有一个活跃任务（`models.py:116-122` 的部分唯一索引），且 `_pump` 是单协程串行循环，所以**单连接不存在并发写放大**；放大来自「同一密钥注册成 N 个 node 行 → N 条连接」。

**4d. 关闭清理 —— 部分成立**

`:close` 会记录断开并回 `bye`（`channel.py:179-183`）；`finally` 用 `shield=True` 保证断开标记不被取消（`channel.py:128-133`），实测 P9/P44/P89 的 `unknown` 标记都正确落地。

**但非文本帧是一条未映射的崩溃路径**。`receive_text()` 在收到 binary 帧时抛 `KeyError('text')`（而不是干净的协议错误；实测 P72 用真实 ASGI 帧复现）：

```
P72: exception=KeyError message='text'
P73: _pump 抛出 KeyError，frames_sent=[]，close_calls=[]
P74: run_channel 捕获（channel.py:126 的 except Exception），只记日志，
     close_calls=[]，disconnect_recorded=[["nod_x","node_disconnected"]]
```

即：节点发一个二进制帧 → 通道被当作「异常结束」，应用**不发送 error 帧、也不显式 close**，只把任务打成 `unknown`。`channel.py:158` 的循环里不存在任何针对非文本帧的分支。客户端最终看到的 close 行为**无法判定**（需要真实 uvicorn + 真实 WS 客户端，本机 TestClient 无法给出；见第五节）。严重性低（正常节点不发二进制帧），但它是「帧类型 → 500/异常」这一类缺陷的实例。

**4e. 跨租户广播 —— 不成立（无缺陷，但身份可换）**

通道消息只发给当前 socket 对应的 node 行（`channel.py:136-152`、`197-210`），不存在广播。但「当前 node 行」由未签名的头决定（项 1c / P99），所以**广播面为零，身份面有缺口**。

---

### 项 5：SQL 拼接、迁移幂等与 down 安全性 —— **部分成立**

**5a. SQL 注入 —— 不成立（无缺陷）**

grep 穷尽 `src` 中的 SQL 字面量：仅 `app.py:126` 的 `text("SELECT 1")`、`models.py:120-121` 的部分索引谓词 `text("status IN ('queued','running')")`。其余全部为 SQLAlchemy Core/ORM 表达式。`claim_queued_task` 的「动态」部分只是常量条件，值仍是绑定参数：

```python
# service.py:867-878
    claimed = await session.scalar(
        update(models.Task)
        .where(models.Task.id == task_id, models.Task.status == "queued")
        .values(
            status="running",
            attempts=models.Task.attempts + 1,
            updated_at=utc_now(),
        )
        .returning(models.Task.id)
        .execution_options(synchronize_session=False)
    )
```

`list_audit` 的 `limit` 在路由层被夹紧（`app.py:490`：`max(1, min(limit, 1000))`）。

**5b. 迁移在非空数据上失败（升级路径不安全）—— 成立**

```python
# migrations/versions/a74d212eb82a_bind_approvals_to_an_action_and_node.py:21-35
def upgrade() -> None:
    # Autogenerated, then made explicit: the foreign key needs a name so the
    # downgrade can drop it, and the new JSON column needs a server default so
    # the batch recreate does not depend on the table being empty.
    with op.batch_alter_table('approvals', schema=None) as batch_op:
        batch_op.add_column(
            sa.Column('node_id', sa.String(length=64), nullable=False)
        )
        batch_op.add_column(
            sa.Column('payload', sa.JSON(), nullable=False, server_default=sa.text("'{}'"))
        )
```

注释明确写了「the new JSON column needs a server default so the batch recreate does not depend on the table being empty」——`payload` 给了 `server_default`，**`node_id` 没给**。在 SQLite 上 `render_as_batch=True`（`migrations/env.py:44-47`）会把「加 NOT NULL 列」实现为「建临时表 → 拷贝 → 改名」，拷贝时 `node_id` 无值 → 失败。

**最小复现**（`services/control-plane/.tmp/repro_migrate.py`；注意必须用 `sqlite+aiosqlite://`，`env.py` 用 `async_engine_from_config`）：

```python
import sqlite3, tempfile, unittest
from pathlib import Path
from alembic import command
from alembic.config import Config

SERVICE = Path(r"D:\yukinal\services\control-plane")

class Repro(unittest.TestCase):
    def test_upgrade_on_non_empty_approvals(self):
        tmp = tempfile.TemporaryDirectory(ignore_cleanup_errors=True)
        db = Path(tmp.name) / "m.db"
        cfg = Config(str(SERVICE / "alembic.ini"))
        cfg.set_main_option("sqlalchemy.url", f"sqlite+aiosqlite:///{db.as_posix()}")

        command.upgrade(cfg, "d371f9a69535")             # node_id 之前的版本
        con = sqlite3.connect(db)
        con.execute("INSERT INTO tenants (id,name,disabled,created_at)"
                    " VALUES ('ten_a','A',0,'2026-10-17 00:00:00')")
        con.execute("INSERT INTO approvals (id,tenant_id,action,fingerprint,state,"
                    "requested_by,requested_at) VALUES ('apr_a','ten_a','server.exec',"
                    "'fp','pending','ada','2026-10-17 00:00:00')")
        con.commit(); con.close()
        try:
            command.upgrade(cfg, "head"); print("ok")
        except Exception as e:
            print(type(e).__name__, str(e)[:120])

unittest.main()
```

实测输出（P60）：

```
IntegrityError: NOT NULL constraint failed: _alembic_tmp_approvals.node_id
[SQL: INSERT INTO _alembic_tmp_approvals (id, tenant_id, task_id, action, fingerprint, state,
 requested_by, decided_by, requested_at, decided_at) SELECT approvals.id, approvals.tenant_id, ...]
```

空表升级正常（P61：`approvals` 最终 17 列齐全）；连续 `upgrade head` 两次正常（P62：幂等，`alembic_version=fc2e168fa85c`）。

**5c. 失败迁移不可重试 —— 成立**

实测 P71：上述失败之后

```
first_attempt="IntegrityError"
tables_after_failure=["_alembic_tmp_approvals", "alembic_version", "approvals", "audit_events",
                      "model_configs", "node_nonces", "nodes", "tasks", "tenants"]
retry="OperationalError: table _alembic_tmp_approvals already exists"
revision="d371f9a69535"
```

即失败会在库中遗留一张 `_alembic_tmp_approvals`，**修复后再跑一次仍会失败**，必须人工 `DROP TABLE`。数据库停留在 `d371f9a69535`，而 `create_app` 的启动检查只比对**表名集合**（`app.py:42-52`），`approvals` 表仍在 → **应用会正常启动，但 schema 落后于模型**（缺 `node_id`/`payload`/`expires_at`/`uses`/...）。这是比迁移失败本身更危险的后果：一个「看起来健康」的旧 schema。

**5d. down 路径安全性 —— 部分成立**

`downgrade base` 在**有数据**时成功（P63：逐级 `c2ee024868fa→a74d212eb82a→d371f9a69535→adf6c5e58f5e→base` 全部 ok，最终只剩 `alembic_version`），且 5 个 revision 都实现了 `downgrade()`。两个保留意见：(1) 删除列即删除数据，这是 downgrade 的固有语义，不算缺陷；(2) `a74d212eb82a` 的 upgrade 在非空数据上不可用，因此「先降级再升级」这条常规回滚再前进的路径，对任何曾含审批数据的库都**断在中间**——downgrade 到 `d371f9a69535` 之后再也升不回来。

`tests/test_control_plane.py:341-391` 的迁移测试覆盖了完整 `downgrade base`、表集合、四个索引、三组外键与 tool_policies 唯一约束——**down 路径本身是被测的**；它没测到 5b/5c，是因为该用例全程在**空表**上迁移（`command.upgrade(config, "head")` 之后直接断言结构）。这是「测试夹具不含数据」导致的漏检，而不是「down 路径无人测」。

**5e. SQLite 运行期外键不生效 —— 部分成立**

```python
# db.py:20-27
def create_engine(database_url: str) -> AsyncEngine:
    """Create the async engine for ``database_url``.

    ``pool_pre_ping`` is on so a dropped PostgreSQL connection (or a restarted
    server) is detected and replaced instead of surfacing a stale-connection
    error to a node.
    """
    return create_async_engine(database_url, pool_pre_ping=True, future=True)
```

`src` 中不存在 `PRAGMA foreign_keys` 或 `connect` 事件监听。实测 P24：`PRAGMA foreign_keys = 0`，插入 `tenant_id='ten_nope'`、`node_id='nod_nope'` 的孤立任务**成功**。迁移里所有 `ondelete="CASCADE"`（`adf6c5e58f5e:39,53,68,87,88,107,108`、`a74d212eb82a:34`、`d371f9a69535:27`、`fc2e168fa85c:32,42`）都只在**能强制外键的后端**上生效。模型层面仍靠应用代码维持一致性（`require_tenant`/`require_node`），所以不是直接越权，但「删租户 → 级联清理」在 SQLite 部署上**不成立**。PostgreSQL 是否强制部分唯一索引（`uq_tasks_active_per_node`，`models.py:116-122`）同样**无法判定**（见第五节）。

**5f. 列长度与校验不一致 —— 部分成立**

`tool_policies.kind` 声明 `String(128)`（`models.py:276-288`），但路由把它当自由路径参数接收（`app.py:367`：`kind: str`，无长度限制），实测 P58：**300 字符的 kind 200 且原样入库**。SQLite 不强制 `VARCHAR` 长度；PostgreSQL 会强制（行为见第五节）。同类：`nodes.agent_version`（`String(64)`）由 schema 限 64 ✓；`audit_events.actor`（`String(200)`）实测 500 字符入库（P57）而 schema 未限长。

---

### 项 6：错误响应泄露内部信息/凭据 —— **成立**

**6a. 校验错误原样回显提交值（含密钥）—— 成立**

`extra="forbid"`（`schemas.py:17-18`）让 `apiKey` 这类未知字段被拒，但 pydantic 的 422 响应体把**提交的值**放进 `input`：

```python
# schemas.py:1-6（模块文档字符串）
"""Request/response models.

Every request model forbids unknown fields, so a caller that sends a raw secret
(for example ``apiKey``) gets a validation error instead of silently having it
dropped or stored.
"""
```

实测 P96（两条路由，提交的 `apiKey` 字符串直接出现在响应体里）：

```json
POST /v1/tenants/{t}/model-configs
422 {"detail":[{"type":"extra_forbidden","loc":["body","apiKey"],
     "msg":"Extra inputs are not permitted","input":"sk-live-<REDACTED-21-CHARS>"}]}
POST /v1/tenants/{t}/approvals
422 {"detail":[{"type":"extra_forbidden","loc":["body","nope"],
     "msg":"Extra inputs are not permitted","input":"sk-live-<REDACTED-21-CHARS>"}]}
```

> **脱敏说明**：上面两处 `input` 的**原值是审查时构造的假值**（合成字符串，绝非真实凭据）：`sk-` 前缀 + `live-` + 三个大写单词用连字符连接，即 `sk-` 后面跟 **21** 个 `[A-Za-z0-9_-]` 字符（总长 24）。本文用 `sk-live-<REDACTED-21-CHARS>` 表示，是为了让仓库的 `node scripts/check-secrets.mjs`（规则 `\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}\b`）保持 green；**只做了字面量替换，证据与结论强度不变**：响应体确实原样回显了请求里的值，回显位置仍是 `detail[].input`（Pydantic `extra_forbidden` 错误的输入回显），可观察形状仍是「`sk-` 前缀 + ≥20 字符的长 token，足以被 `check-secrets` 自身的规则命中」——也就是说，**这个回显面连仓库自己的密钥扫描器都会误判为真实凭据**，这反而加强了 6a 的结论。

`input` 里回显的是**整个提交值**，所以「把密钥放在错误字段里」的无效请求，会让密钥出现在响应体、调用方日志、以及任何捕获 4xx body 的中间件/代理里。这一条与模块文档字符串声称的目的（避免凭据被「dropped or stored」）方向一致，但**多了一个回显面**。同类：类型错误也会回显（P28：`kind: 42` → `"input":42`）；深层嵌套的完整提交体同样可能被回显。真实凭据进入 `input` 的前提是调用方**自己**把它放在一个未知字段/错误类型里（例如把 `apiKey` 而不是 `api_key_ref` 传给 `POST .../model-configs`）——这正是 6a 的威胁模型。

**6b. 节点认证失败原因可区分（5 种消息）—— 成立**

`authenticate_node` 的文档字符串承诺「An unknown node, a stale timestamp, a bad signature and a replayed nonce all raise the same ``Unauthorized``: the endpoint never confirms which node ids exist, nor which half of the check failed」（`service.py:193-198`），但实现给出了 5 条可区分消息（`service.py:200,204,206,211,225`）：

```
P93 unknown_node                    -> 401 "node authentication failed"
    existing_node_wrong_key         -> 401 "the request signature is not valid for this node"
    existing_node_stale_timestamp   -> 401 "the signed timestamp is outside the accepted window"
    no_headers                      -> 401 "node authentication headers are missing"
    replayed_nonce                  -> 401 "this request nonce has already been used"
```

**确实无法区分「节点是否存在」**（未知节点与已禁用租户的节点都是同一句，P94：禁用租户 `401 node authentication failed`，未知节点 `401 node authentication failed`）——这一点文档承诺是成立的。但**可以区分「签名对不对 / 时间戳是否越界 / nonce 是否被用过」**，文档承诺的「nor which half of the check failed」不成立。这不构成身份枚举（node id 随机生成），但把「nonce 是否已消耗」「时间戳窗口」等内部状态暴露给了未鉴权方——正是项 2b 里攻击者需要在 300 s 窗口内反复探针的信息。

同一细节还通过 WS 的 close reason 外泄（P95）：

```
unknown_node            -> [4401, "node authentication failed"]
existing_node_wrong_key -> [4401, "the request signature is not valid for this node"]
```

`channel.py:114` 直接 `reason=str(error)`。

**6c. 未处理的异常返回裸 500 —— 部分成立（非泄露）**

深度嵌套 JSON 在**已鉴权**时触发 `RecursionError`，返回 `500 Internal Server Error`（`text/plain`，Starlette 默认，无栈、无内部细节）：

```
P79 authed=[500, "text/plain; charset=utf-8", "Internal Server Error"]
    unauthenticated=[401, "{\"detail\":\"unauthorized\"}"]
    wrong_token=[401, "{\"detail\":\"unauthorized\"}"]
P80 first_5xx_depth=1600     （1200/1600 层嵌套触发；20000 层在解析阶段就被拒）
```

**这不是信息泄露**（body 是固定的哑 500），而是健壮性问题：鉴权后的调用方可用约 3 KB 的请求体换取一个 500。注意 `IntegrityError` 被显式映射成通用 409（`app.py:115-122`），说明作者已经在做「不要带着驱动文本返回 500」的处理，`RecursionError` 属于漏网。

**6d. 认证与 body 解析的先后 —— 混合，值得记录**

实测：

| 情形（无 token） | 结果 |
|---|---|
| 合法 JSON、schema 非法（`extra_forbidden` / 类型错 / 缺必填） | **401**（鉴权先赢） |
| 非法 JSON（解析失败，含 2 MiB 截断体） | **422 `json_invalid`**（在鉴权之前就报错） |
| 合法但非对象的 JSON（`[1,2,3]`、`null`） | **401** |
| 2 MiB 合法 JSON | **401**，耗时 0.004 s |
| 深层嵌套 JSON（1200 层） | **401**（未鉴权不会触发 500） |

即：**JSON 解析发生在鉴权之前，schema 校验发生在鉴权之后**。实测 P100（2 MiB 截断体，`loc: ["body", 2097230]`）在**未鉴权**时也返回 422，说明未鉴权的对端可以驱动服务完整读取并解析最多 2 MiB 的请求体；同时响应体里回显了 `input: {}`（解析失败时无原值，故未泄露内容）。合法 2 MiB body 则快速 401。P85 确认无 token 时密钥不会回显（`401 {"detail":"unauthorized"}`），所以 6a 的回显面**要求持有 token**。

**6e. 明文 HTTP + 文档端点公开 —— 部分成立**

`/docs`、`/redoc`、`/openapi.json`、`/docs/oauth2-redirect` 全部**无鉴权可读**（P2/P19：`200`），`/v1/metrics` 需要 token（`401`）；OpenAPI 里没有声明 `securitySchemes`。文档端点公开本身是常见取舍，但配合「无 TLS 终止由部署方负责」（`config.py:24-27`：注释明确要求部署方终止 TLS）意味着**默认本地开发配置下，bearer token 与节点签名头都走明文**。是否在生产前面有 TLS 终止**无法判定**（见第五节）。

---

### 项 7：并发、事务边界与竞态 —— **部分成立**

**7a. 抢占类竞态 —— 不成立（无缺陷，实测安全）**

- **nonce**：复合主键 + `flush` 抛 `IntegrityError` → `Unauthorized`（`service.py:216-232`）。
- **审批消耗**：`UPDATE ... WHERE uses < max_uses`（`service.py:754-769`）。实测 P7/P45：2/3 个并发消耗同一单次审批 → 一个 `True`、其余 `False`（顺序不定），最终 `uses=1/max_uses=1`。
- **任务槽位**：部分唯一索引 `uq_tasks_active_per_node`（`models.py:116-122`）+ `try/except IntegrityError → Conflict`（`service.py:966-969`）。实测 P46：6 线程并发创建 → `[409,409,201,409,409,409]`，最终 `task_rows=1`。
- **queued 认领**：条件更新（`service.py:858-878`），单节点单槽位下并发 `move running` 都映射到同一条合法边。

**7b. 并发下的审计放大 —— 轻微**

`P69`：4 个并发 `queued→running` 全部返回 200，任务最终 `running/attempts=1`，但审计里出现 **4 条 `task.transition`（`queued→running`）**。原因：`transition_task` 先 `require_task` 读行（不锁），4 个请求都看到 `queued`，各自写 `running`（幂等覆盖）并各记一条审计。状态机没有被破坏（最终状态正确、`attempts` 只加 1，因为 `attempts += 1` 在 ORM 层基于各自读到的 0 → 都写 1），但**审计事件数与实际状态变更数不符**，会干扰「审计即事实」的推断。P70（3 个并发终态迁移 succeeded/failed/cancelled）结果 `[200,200,200]`，最终 `failed`——同样多记 2 条不存在的迁移。

严格说这是「lost update 在审计维度上的表现」：任务状态列被后写覆盖，`attempts` 恰好因为都是 0→1 而未失真，但在 `attempts` 已经 >0 的并发场景下可能低估重试次数。建议把状态迁移也做成条件更新（`WHERE status = previous`）或对任务行加行锁。

**7c. 失败请求不烧 nonce —— 见项 2b（同一事务边界的另一面）**

`app.py:60-63` 的 `get_session` 把事务边界交给依赖生成器：

```python
# app.py:60-63
    async def get_session() -> AsyncIterator[AsyncSession]:
        async with session_factory() as session:
            async with session.begin():
                yield session
```

路由抛 `ControlPlaneError` → 生成器在 `yield` 处收到异常 → `session.begin()` 回滚 → nonce 插入一并回滚。**这是「失败请求不留痕」的正面性质（不留半成品）与「失败请求不消耗 nonce」的负面性质共用一个机制**。修复不应放弃回滚，而应把 nonce 写入放到独立提交的短事务里（见第六节）。

**7d. async session 复用 —— 不成立（无缺陷）**

每个请求一个 session（依赖注入），WS 每条消息开新 session（`channel.py:138-152`、190-194、197-210、228-240），不存在跨请求复用；`session_factory` 是 `async_sessionmaker(expire_on_commit=False)`（`db.py:30-31`），配合路由内 `await session.flush()` + 提交后读取 `populate_existing=True`（`service.py:919`）避免过期实例问题。未发现 async session 跨任务共享。

**7e. 多进程/PostgreSQL —— 无法判定**

见第五节。本地只有 SQLite；`pool_pre_ping=True` 与 asyncpg 的并发语义、行锁与部分唯一索引行为无法在本机验证。所有「条件更新已足够」的结论都基于**单进程 + SQLite + TestClient** 的实测，代码结构上支持多 worker（条件更新不依赖 Python 状态，`node_nonces` 刻意落库就是为了跨 worker），但**未在多 worker 上验证**。

---

## 三、附加发现（威胁模型第 7 项之外）

### F-1 密钥字段边界只覆盖 task payload（Medium）

```python
# app.py:247
        assert_no_secret_fields(body.payload)
```

grep 穷尽：`assert_no_secret_fields` / `find_secret_fields` 在整个 `src` 里**只有这一个调用点**。而下列路径都接受并持久化自由 `dict`：

- `POST .../approvals` 的 `payload`（`schemas.py:165`：`payload: dict = Field(default_factory=dict)`），响应模型原样回显（`schemas.py:207`：`payload: dict`）
- `ModelConfigCreate.api_key_ref`（`schemas.py:139`，别名 `apiKeyRef`）——`_normalized("apiKeyRef") == "apikeyref"` **包含** `apikey` 这个提示词，也就是说**如果** `assert_no_secret_fields` 在这一层被调用，它本来会拦住这个字段名。它没被拦住，纯粹是因为 `security.py` 在这条路径上从未被调用（`app.py:306-313` 直接透传 `body.api_key_ref`），而 `api_key_ref` 是**已声明字段**、不受 `extra="forbid"` 约束。实测 P97：`apiKeyRef: "<api-key-shaped-fake>"` → 201 并原样回显。
- 节点上报的 `result`（HTTP：`app.py:279` → `schemas.py:112`；WS：`channel.py:239`）

实测（下表中的凭据值均为审查时构造的**假值**，非真实凭据；写作时已改写成不触发 `check-secrets` 规则的占位符，形状与长度保持可核对）：

```
P3  task payload     {"password":"hunter2"}          -> 400 secret_rejected（值未被回显）
    approval payload {"password":"hunter2"}          -> 201，payload 原样回显
    apiKeyRef        "<api-key-shaped-fake>"         -> 201，api_key_ref 原样回显
    中性字段名        {"creds":{"pass":…,"key":…}}    -> 201 通过（启发式只看字段名）
    task result      {"password":"hunter2","apiKey":"<sk-prefixed-fake>"} -> 200 持久化并回显
```

`_SECRET_HINTS`（`security.py:14-24`）匹配的是**字段名**，且 `find_secret_fields` 只在 `dict`/`list` 上递归——`apiKeyRef` 这个「引用」字段名恰好命中了 `apikey` 提示词，但因为没被调用而无关。这是**保护面比设计意图窄**，不是直接越权；风险是「把凭据写进注册表并回显」，一旦数据库或响应被读取即为泄露。

### F-2 `result` 无大小上限（Low-Medium）

`schemas.py:112`：`result: dict | None = None`（无 `max_length`），`models.py:144`：`result: Mapped[dict | None] = mapped_column(JSON)`（无长度）。实测 P41c：200 KB 的 `result` → `200`，`stored_result_len=200009`。对照 `outcome` 有 `max_length=64`（`schemas.py:111`），5000 字符 → 422。任意租户可无界膨胀 `tasks.result`（SQLite 无列长约束），且该字段会被列表/详情接口整份回显（`TaskView.result`）。

### F-3 tool-policy `kind` 无长度上限（Low）

见 5f。`PUT /v1/tenants/{t}/tool-policies/{kind}` 对 `kind` 无任何校验（`app.py:370-383`），实测 300 字符可写入 `String(128)` 列。

### F-4 `X-Yukinal-Actor` 由调用方自填且无长度上限（Medium，审计可信度）

```python
# app.py:75-76
        actor = (x_yukinal_actor or "operator").strip()
        return actor or "operator"
```

审计里的 `actor` 完全来自请求头（实测 P14：同一个 token 可以既请求又批准，审计中出现自填的 `victim-requester` / `victim-approver`；P57：500 字符原样入库到 `String(200)` 列）。这让「谁批准了什么」在审计上不可信——**在只有一个共享 token 的模型下，`actor` 是唯一的操作者标识**，却没有任何绑定或校验。

### F-5 SQLite 外键不生效（Medium，见 5e）

### F-6 `/docs`、`/openapi.json` 公开（Low）

### F-7 节点公钥无唯一约束（Medium，见 1c）

### F-8 深度嵌套 JSON 触发未处理 500（Low，见 6c）

### F-9 非文本 WS 帧走未映射崩溃路径（Low，见 4d）

### F-10 无 TLS 强制（无法判定，见第五节）

---

## 四、严重性排序的修复清单

按「先修可被利用的授权绕过，再修纵深层，最后修收窄项」排序。

**P0 —— 授权绕过（建议与上述发现同批修复）**

1. **把 node 身份纳入签名**（修 1c，约 5 行）。两种等价做法任选：
   - 规范串加入 node id：`canonical_request(..., node_id=...)`，把 `node_id` 作为第 1 行或末行；`nodeauth.sign_headers` 同步（客户端契约变更，需 bump 版本）。
   - 或对 `nodes.public_key` 加**全局唯一约束**并拒绝重复注册（`models.py:59` 加 `UniqueConstraint("public_key")` + 新 revision；`service.py:133-136` 的 `IntegrityError` 分支已存在，映射消息要改）。此法不改变签名协议，但要处理存量重复数据。
   两者都做最稳妥：唯一约束消除歧义，签名绑定消除「同一密钥两行」的等价性。
2. **补全派发门禁**（修 3b/3c，约 10 行）：在 `task_authorisation_problem` 里补三处比对，并把门禁覆盖到所有进入 `running` 的边：
   ```python
   # service.py:832 之后
   if approval.tenant_id != task.tenant_id or approval.node_id != task.node_id:
       return "approval_mismatched"
   if approval.fingerprint != actions.action_fingerprint(
           kind=task.kind, node_id=task.node_id, payload=task.payload or {}):
       return "approval_mismatched"
   ```
   同时把 `service.py:1028` 的条件从 `status == "running" and previous == "queued"` 改为 `status == "running"`（`unknown→running` 同样需要授权）。**注意**：改这一条会影响 `tests/test_policy.py`、`tests/test_approvals.py` 中依赖现有行为的用例，需一并评估——修改测试行为时要区分「修正断言」与「放宽断言」。
3. **让 nonce 在失败的请求上也失效**（修 2b，约 15 行）：把 nonce 的写入/校验放进**独立提交的短事务**（`authenticate_node` 内自建 session 或在 claim 后立即 `commit()`），或在路由层抛错时**不回滚 nonce**（例如把 nonce 表写在一个不参与请求事务的连接上）。任一方案都必须保持「同一 nonce 二次使用 → 401」。同时补 nonce 长度上限（`service.py:199` 处加一个显式长度检查，返回 401/422，与 `String(128)` 对齐），并**拒绝含控制字符的 nonce**（P106/P107 证明 `\n` 可穿过 HTTP 层并被用于签名，是 2a 边界平移弱点的唯一可用输入）。
4. **启用禁用租户的全面熔断**（修 1b，约 6 行）：让 `require_node`、`require_task`、`create_approval`/`decide_approval`/`revoke_approval`/`expire_stale_approvals` 都经过 `require_tenant`（或抽一个 `require_active_tenant` 并在这几处调用）。这会让 `tests/` 中涉及这些路由的用例保持一致（它们目前都不禁用租户，预期无破坏）。

   **需覆盖的具体 10 条路由**（第一节矩阵）：`#7 GET .../nodes/{n}`、`#8 POST .../nodes/{n}/heartbeat`、`#9 GET .../nodes/{n}/recoverable`、`#11 POST .../tasks`、`#12 GET .../tasks/{id}`、`#13 POST .../tasks/{id}/status`、`#23 POST .../approvals`、`#24 POST .../approvals/{id}/decide`、`#25 POST .../approvals/{id}/revoke`、`#26 POST .../approvals/expire`。

   其中 **#26 优先**：它的调用链上完全没有任何租户存在性/禁用校验（`service.py:675-715`），实测对**不存在**的租户也返回 `200 {"expired":0}`（P119），对**已禁用**的租户会真的把 `approved` 改成 `expired` 并写入审计（P122，`{"expired":1}`）——即 `disabled` 开关挡不住这条写入路径。修法是在 `app.py:478` 调用前加 `await service.require_tenant(session, tenant_id)`（其余 9 条同理，在条目级校验前补一次 `require_tenant`）。

   **注意不要过度修正**：#26 对不存在租户的 200 与对已禁用租户的 200 是**同一根因**（无门禁），不是「存在性预言机」——P123 显示启用/禁用/不存在三种租户的响应逐字节相同，修完应保持「不存在租户 → 404」而不是试图把它变成「迷惑性一致响应」。

**P1 —— 纵深与一致性**

5. **迁移 `a74d212eb82a` 补 `server_default`**（修 5b/5c）：给 `node_id` 一个可回填的默认值。真正的正确做法取决于业务语义——现有审批行确实有对应的 node 吗？`approvals` 在 `d371f9a69535` 时已有 `task_id`（可空），所以可回填路径是：`node_id` 先加为 nullable → `UPDATE approvals SET node_id = (SELECT node_id FROM tasks WHERE tasks.id = approvals.task_id)` → 剩下的 NULL 行需要有明确的处理策略（删行 / 填哨兵值 / 迁移中止并报告），然后再加 NOT NULL。**这一条需要产品决策（找不到 node 的历史审批怎么处理），不能机械地用 `server_default` 掩盖**。同时在迁移失败后自动清理临时表（或在文档中给出 `DROP TABLE _alembic_tmp_approvals` 的恢复步骤）。
6. **启动检查从「表名集合」升级为「列集合」**（修 5b 的后遗症）：`app.py:42-52` 只比对表名，所以停在 `d371f9a69535` 的库会「健康启动」。建议比对 `alembic_version` 与 `ScriptDirectory.get_current_head()`，不一致即拒绝启动。这会**立刻**暴露任何升级失败的部署。
7. **密钥字段边界扩展到所有自由 `dict`**（修 F-1，约 10 行）：把 `assert_no_secret_fields` 提为共用依赖/中间件，覆盖 `create_approval.payload`、`TaskStatusUpdate.result`、WS `status.result`，并对 `api_key_ref` 增加「必须是引用而非裸凭据」的形状校验（至少拒绝 `sk-`/`-----BEGIN` 这类前缀，与 `security.py` 已声明的「store a credential reference」意图一致）。
8. **`X-Yukinal-Actor` 收紧**（修 F-4）：加长度上限（与 `String(200)` 对齐），并在文档中明确它不是可信身份；若需要真实的操作者区分，应引入每租户 token 或签名主体，而不是客户端自填头。

**P2 —— 收窄与健壮性**

9. `result` 加大小上限（响应体与 DB 双层），与 `outcome` 的 64 字符对称（修 F-2）。
10. `kind` 路径参数加长度上限（复用 `schemas.py:103` 的 `max_length=64` 或 tool-policy 的 128）（修 F-3）。
11. 把 WS 大小检查移到**读取之前**或用分块读取，使「refused rather than buffered」与实现一致；同时为 `channel.py:158` 增加显式的「非文本帧 → error 帧 + 明确 close」分支（修 4b/4d）。
    - **11.1 把 WS 的状态上报改用与 HTTP 相同的 pydantic 模型**（修 4b 的校验不一致，P102）：`channel.py:213-245` 目前手工 `isinstance` 过滤，`outcome` 无 64 字符上限、`result` 无大小上限。直接复用 `schemas.TaskStatusUpdate.model_validate(message)`，校验失败回 `error` 帧。这样 HTTP 与 WS 两条入口对同一份数据施加同一套约束。
    - **11.2 规范串改用长度前缀或结构化编码**（修 2a 的结构性弱点，防御性）：`nodeauth.canonical_request` 目前用裸 `\n` 连接 5 个字段。改为 `f"{len(value)}:{value}\n"` 形式后，即使将来某个字段能含 `\n` 也不会产生歧义。当前不可利用（P112/P115 已测量），因此排在 P2。
    - **11.3 `/docs`、`/redoc`、`/openapi.json` 加鉴权或按环境关闭**（修 F-6）：`FastAPI(docs_url=None, redoc_url=None, openapi_url=None)` 或仅在非生产环境挂载。当前无鉴权可读（P2/P19）。
12. 统一节点鉴权失败消息（修 6b）：把 5 条消息收敛成 1–2 条（「认证失败」/「已拒绝」），至少不要区分 nonce 已用与签名错误；WS 的 close reason 用固定文案。
13. 抑制 pydantic 422 响应里的 `input` 回显（修 6a）：自定义 `RequestValidationError` 处理器，剥掉 `input`（或仅保留 `loc`/`type`/`msg`）。这会同时消除 F-1 的外泄面。
14. `RecursionError` 加一层 handler → 400/413，而不是裸 500（修 6c）。
15. SQLite 引擎加 `PRAGMA foreign_keys=ON` 的 connect 事件（修 5e/F-5）；生产用 PostgreSQL 时确认 `uq_tasks_active_per_node` 的部分索引已建立。
16. 状态迁移改为条件更新或行锁，使审计事件数等于真实状态变更数（修 7b）。
17. 运维文档补一句：SQLite 仅用于开发，生产必须 PostgreSQL + TLS 终止（`config.py:24-27` 已有注释，但缺「SQLite 下外键/列长不强制」这条）。

---

## 五、无法判定项（缺证据，不计为缺陷）

以下每条都给出**所缺证据**；在拿到该证据前不下结论。

| # | 事项 | 所缺证据 |
|---|---|---|
| U-1 | PostgreSQL/asyncpg 下外键、级联、部分唯一索引是否强制；并发条件更新在 real DB 上的行为 | 需要一个可用的 PostgreSQL 实例跑同一批探针。本机只有 SQLite（`aiosqlite`），`postgres` extra 的 `asyncpg` 未安装/未连通 |
| U-2 | `varchar(128)`/`varchar(200)` 在 PostgreSQL 上的越界行为（是干净的 4xx 还是驱动错误 → 500）；实测 P68/P57/P58 的长值在 PostgreSQL 上的结果 | 同 U-1。SQLite 不强制长度，因此不能外推 |
| U-3 | 多 worker/多进程下 queued 认领与审批消耗是否仍然无竞态 | 需要 `uvicorn --workers N` 或 CTest 之外的多进程编排 + 真实 DB；本次全部为单进程 TestClient |
| U-4 | 真实网络下 WS 背压行为（`drain` 语义、慢消费者、单连接内存上界） | 需要真实 uvicorn + 真实 WS 客户端；TestClient 的 `websocket_connect` 不走网络栈，无法观察背压。`uvicorn` 默认 `ws_max_size=16 MiB` 已确认（P77），但「应用读之前缓冲多少」取决于服务器实现 |
| U-5 | 非文本 WS 帧时客户端最终观察到的 close code（1000/1006/1011） | 同 U-4；P73/P74 只能证明应用既不 close 也不回 error 帧 |
| U-6 | 生产部署是否在控制平面前终止 TLS | 需要部署清单/反向代理配置/`acceptance-handoff.md` 的环境说明；仓库内只找到 `config.py:24-27` 的注释要求 |
| U-7 | 是否存在**纯 HTTP** 的 `tasks.approval_id` 改写路径（3b 的可利用性前提） | 需要对全仓库（含 TS/C++ 客户端、运维脚本、未来迁移）做写入面穷尽；本次只审了 control-plane 源码范围内，API 侧确认无入口（P90 的 A 租户无审批时创建被拒），但数据库直连/迁移/外部工具不在审查面内 |
| U-8 | `0.1.0` 之外是否有既存部署、是否已有含审批数据的库需要 5b 的数据回填策略 | 需要部署历史/线上 schema 版本；仓库内无相关记录 |
| U-9 | 含 `\n` 的 `X-Yukinal-Nonce` 在**真实 HTTP/1.1 传输**上是否可达（P106 的 LF 穿透是在 TestClient 的 ASGI 直调路径上测得的，未经过 HTTP 报文解析器；真实服务器/中间代理通常拒绝或改写含裸 CR/LF 的头，那既可能是协议走私面，也可能让 2a 的边界平移弱点彻底不可达） | 需要真实 uvicorn（或目标反向代理）+ 原始 socket 发送含裸 LF 的头，观察 400 / 头拆分 / 直通三种结果。当前只能确认「应用层不做控制字符校验」（`service.py:199` 仅检查非空） |

---

## 六、结论（给 Lead 的判定）

- **项 1 多租户隔离** —— 部分成立。操作员侧不存在按租户授权（单一共享 token，设计属性）；**10 条**含 `{tenant_id}` 的路由绕过 `disabled`（#7/#8/#9/#11/#12/#13/#23/#24/#25/#26，判据与实测见第一节「怎么算出来的」，全局路由 #1/#2/#3 不计入）；跨租户读写在「同一公钥登记进两个租户」的前提下**可复现**（P98/P99/P66）。
- **项 2 签名规范形与重放** —— 部分成立。规范形在当前字段组合下无歧义（P27/P112/P115 三重搜索无碰撞，不成立为缺陷；结构性边界平移弱点已被测量关闭，但建议加长度前缀加固）；**重放成立**：失败请求不烧 nonce，300 s 窗口内可反复重放且状态变化后由拒转变成功（P82/P83）；nonce 无长度上限（P68）。
- **项 3 审批绑定** —— 成立。创建侧绑定正确、消耗原子（P7/P45）；派发侧不比对租户/node/指纹（P90/P91/P92），且 `unknown→running` 完全跳过门禁（P87/P88/P89，纯 HTTP 可利用，违反「deny 绝对」的自身不变量）。
- **项 4 WebSocket 通道** —— 部分成立。握手 `accept` 前鉴权 ✓、大小上限 ✓、未知消息拒绝 ✓、断开清理 ✓；但大小检查在缓冲之后、无速率限制、无非文本帧分支（P72/P73/P74）、close reason 泄露细节。
- **项 5 SQL 与迁移** —— 部分成立。无 SQL 拼接（不成立为缺陷）；`a74d212eb82a` 在非空 `approvals` 上失败且失败后不可重试（P60/P71）；空表 downgrade base 正常（P63）；启动检查只看表名（`app.py:42-52`）。
- **项 6 错误信息泄露** —— 成立。422 原样回显提交值（含密钥，P96）；节点认证 5 条可区分消息 + WS close reason（P93/P95）；裸 500 无细节泄露（P79）。
- **项 7 并发** —— 部分成立。条件更新使 nonce/审批/任务槽位抢占安全（P7/P45/P46/P59）；剩余问题是非烧 nonce 窗口（项 2）与并发审计重复（P69/P70）；PostgreSQL/多进程**无法判定**。

**是否存在可复现的跨租户或重放路径：是，两者都存在。**

- **重放**：确定存在。不依赖任何前置条件——任何被捕获的节点请求（在其 300 s 签名窗口内）都可在失败后被反复重放，并在系统状态变化后成功（P82/P83）。修复：P0-3。
- **跨租户**：确定存在，但**有前置条件**——同一把 Ed25519 公钥必须同时登记在两个租户的 `nodes` 行内（schema 允许，`models.py:59,68` 无唯一约束；API 允许，P56）。满足该条件时，仅改未签名的 `X-Yukinal-Node` 头即可读另一租户任务（P66/P99）、并可**改写另一租户任务的状态与结果**（P98：`200`，`status=running`，`outcome/result` 被写入），无需目标租户的任何凭据。修复：P0-1。
- 不满足该条件时，未发现纯 HTTP 的跨租户读写（A 租户无法把自己的任务挂到 B 的审批上，创建即 409；节点路由的 `node_id`/`tenant_id` 归属校验挡住了直接指定）。

**最高优先级的三条**：(1) 签名不绑定节点身份 + 公钥可复用 → 条件性跨租户写（P98）；(2) `unknown→running` 绕过派发门禁 → 撤销/`deny` 可被绕过（P87/P88，纯 HTTP）；(3) 失败请求不烧 nonce → 300 s 窗口内可重放并等待状态变化（P82）。

---

## 七、修复记录（FIX-5）—— 上述两条 P0 的落地

本节记录**本次对本文档所列缺陷的修复**。前六节是对**修复前那个 revision** 的审查快照，其结论、行号与 P 编号保持原样不追改；两者冲突时，以本节为准（并注明修复后的行号）。

修复范围严格限定为条目 4 中优先级最高的两条（P0 #3 的 nonce 部分与 P0 #4 的 `#26`），其余各条（P0-1 签名绑定、P0-2 派发门禁、P1/P2 全部）**未动**。

### 7.1 修 2b：nonce 改为独立事务提交（本文档 P0-3）

**选定变体：独立提交的短事务**（`service.py:247-254`），而非「claim 后立即 `session.commit()`」。理由是实测的：后者虽然让 nonce 行活了下来，但会让请求自身的 `session.begin()` 上下文管理器失效（`InvalidRequestError: Can't operate on closed transaction inside context manager`），即请求内后续任何写入都会失败——不能作为修复。

实现（`service.py:231-261`）：

```python
async def claim_nonce(
    session: AsyncSession,
    *,
    nonce_session_factory: async_sessionmaker[AsyncSession],
    node_id: str,
    nonce: str,
    now: datetime | None = None,
) -> None:
    stamp = now or utc_now()
    async with nonce_session_factory() as nonce_session:
        async with nonce_session.begin():
            nonce_session.add(models.NodeNonce(node_id=node_id, nonce=nonce, at=stamp))
            try:
                await nonce_session.flush()
            except IntegrityError as error:
                raise Unauthorized("this request nonce has already been used") from error
    await session.execute(delete(models.NodeNonce).where(...))   # 窗口外清理仍走请求事务
```

**由此产生的顺序保证（必须明写）**：nonce 现在在**验签之前**占用，且占用事务与请求事务**相互独立**。因此两类重放都被覆盖——本文档 P0-3 记的两类：签名校验失败的重放（P83 类），以及「签名通过但路由失败」的重放（P82 类）。代价是**多一次数据库往返**（每请求一次短事务）；在 SQLite 上实测 16 个并发签名请求 0.20 s 全 200（P202），且非空/重复 nonce 仍由 `(node_id, nonce)` 复合主键串行化（P201：两个并发同 nonce 请求 → `[200, 401]`）。

**刻意保留的两个「不烧」**：`authenticate_node` 在能解析出**活着的节点**之前就拒绝的两条路径——缺头（`service.py:209-210`）与未知/禁用节点（`service.py:213-214`）——不写 nonce。原因是此时没有可归属的 `node_id`（`node_nonces.node_id` 有指向 `nodes.id` 的外键且为复合主键的一部分），硬写需要哨兵值，而且会让未认证请求获得填表能力。这条边界由新测试 `test_a_request_without_headers_spends_no_nonce` / `test_an_unknown_node_spends_no_nonce` 钉住。

**清理时机的小变化**：窗口外行的清理仍执行在请求事务里（`session.execute(delete(...))`，`service.py:255-261`）。它现在**晚于**占用（原本在同样位置且同事务），这是有意的——清理失败只影响表大小，不应影响占用；且若把清理也放进独立事务，就会在「同一请求」上再开一次写事务。

**签名变更（内部 API）**：`authenticate_node`(service.py:182-193) 与 `claim_nonce`(service.py:231-238) 增加必填参数 `nonce_session_factory`。两个调用点已同步：`app.py:97-107`（`current_node` 依赖，传入 `create_app` 里的 `session_factory`）与 `channel.py:94-103`（WS 握手，传入 `_authenticate` 已有的 `session_factory`）。选择显式传工厂而不是从入参 `session` 反推（`AsyncSession.bind` 在 SQLAlchemy 2.1 可用），是为了让「这次写入**必须**独立提交」在调用点就可见，而不是靠 `session` 的内部绑定状态。

### 7.2 修 1b 的 `#26`：`expire` 补租户门禁（本文档 P0-4 中优先项）

`app.py:485`：在 `expire_stale_approvals` 之前加一行与所有兄弟路由相同的门禁：

```python
await service.require_tenant(session, tenant_id)
expired = await service.expire_stale_approvals(session, tenant_id=tenant_id)
```

因此不存在/已禁用租户现在返回与兄弟路由**逐字节相同**的 `404 {"code":"not_found","message":"tenant `…` was not found"}`（由新测试 `test_the_sweep_of_a_missing_tenant_is_not_found` 直接比对 `GET /v1/tenants/{id}` 的响应体断言）。**没有**把响应改成「迷惑性一致」：修的是门禁缺失，不是可观测性，P123 的结论（修复前三种租户响应逐字节相同、因此**不构成**存在性预言机）仍然成立。

对已禁用租户，新测试 `test_the_sweep_of_a_disabled_tenant_is_refused_and_touches_nothing` 断言 404 **并且**该审批仍为 `approved`、`audit_events` 行数不变、无 `approval.expired`——即 404 不是「先写后拒」。

新增 `test_the_sweep_still_runs_for_an_enabled_tenant` 反向钉住「没有过度修正」：启用租户的过期审批仍被改为 `expired`、仍写入恰好一条 `approval.expired`、且仍幂等（第二次 `{"expired": 0}`）。

### 7.3 仍未修复（避免误读为已关闭）

- 条目 4 中的**其余 9 条**禁用租户路由（`#7/#8/#9/#11/#12/#13/#23/#24/#25`）**未动**，第一节矩阵的「禁用租户仍可用 = 10 条」现在应为 **9 条**（`#26` 已修）。
- P0-1（签名不绑定 node id + `public_key` 可复用）与 P0-2（`unknown→running` 绕过派发门禁）**未动**——这两条仍是最高优先级的开放缺陷，也仍是「条件性跨租户写」与「撤销/`deny` 可被绕过」的唯一成因。
- nonce 仍**无长度上限**、仍**不拒绝控制字符**。注意：修 2b **扩大了**这一条的影响面——修复前 nonce 只在验签通过后才写入，所以「长度不限」只有已认证的节点能用；修复后占用发生在验签之前，于是**任何知道某个 node id 的人**（node id 不是秘密，且 P98 已证明跨租户可知）都能用垃圾签名写入任意长度的 nonce 行。实测 P501/P503：200 次垃圾签名请求 → 200 行、7.0 ms/次；单个 100 000 字符的 nonce 在 SQLite 上被原样存下（`MAX(LENGTH(nonce)) = 100000`）。**这是本次修复引入的代价，不是原有缺陷**，因此单列在 7.7 并给出最小修法建议；本次严格限定只改两条 P0，未顺手加护栏。
- P1/P2 全部条目未动。

### 7.4 修复后的回归证据（本次运行）

```powershell
cd D:\yukinal\services\control-plane
& .\.venv\Scripts\python.exe -m unittest discover -s tests -p "test_*.py"
# Ran 139 tests in 11.618s
# OK
```

测试数从 **129 → 139**（+10 条新用例）。其中直接对应两条缺陷的 5 条要求：`test_the_sweep_of_a_missing_tenant_is_not_found`、`test_the_sweep_of_a_disabled_tenant_is_refused_and_touches_nothing`、`test_the_sweep_still_runs_for_an_enabled_tenant`、`test_a_nonce_refused_with_a_bad_signature_is_still_spent`、`test_a_nonce_is_not_released_by_a_request_the_route_refused`；第 4 条要求（成功后同 nonce 仍被拒）由既有 `test_a_nonce_cannot_be_reused` 原地**加强**（补断言「恰好一行 nonce」），未新增用例。其余 5 条钉住边界与相邻行为：`test_a_disabled_tenant_is_refused_by_every_tenant_route`、`test_a_nonce_is_burned_before_the_signature_is_checked`、`test_a_request_without_headers_spends_no_nonce`、`test_an_unknown_node_spends_no_nonce`、`test_a_handshake_that_fails_after_the_nonce_is_burned_stays_burned`。分布：`tests/test_node_auth.py` 16→21（+5）、`tests/test_approval_lifecycle.py` 23→27（+4）、`tests/test_node_channel.py` 9→10（+1）；其余文件未动（AST 独立重算：23+20+38+21+10+27 = 139 ✓）。修复后的实测量见 P201–P205（`.tmp/fix5_probe3.py`）。

### 7.5 差异回归：新用例在修复前**确实失败**

只证明「修复后全绿」不足以说明这些用例守住了缺陷。用 `git show HEAD:` 把修复前的 `app.py`/`service.py`/`channel.py` 还原到 `.tmp/before/`，让**同一份测试套件**跑在修复前的源码上（`.tmp/fix5_probe4.py`，`PKGFILE` 已确认加载的是 `.tmp/before/` 那份包）：

```text
P301 restored pre-fix app.py / service.py / channel.py from HEAD
PKGFILE …\.tmp\before\yukinal_control_plane\__init__.py
Ran 139 tests in 11.715s
FAILED (failures=7)
```

7 条失败**恰好**是本轮新增（或加强）的用例，且全部落在两条缺陷上：`test_the_sweep_of_a_missing_tenant_is_not_found`、`test_the_sweep_of_a_disabled_tenant_is_refused_and_touches_nothing`、`test_a_disabled_tenant_is_refused_by_every_tenant_route`（`subTest` 中只有 `…/approvals/expire` 这一条路径失败，正是 `#26`）、`test_a_nonce_refused_with_a_bad_signature_is_still_spent`、`test_a_nonce_is_not_released_by_a_request_the_route_refused`、`test_a_nonce_is_burned_before_the_signature_is_checked`、`test_a_handshake_that_fails_after_the_nonce_is_burned_stays_burned`。其余 132 条在修复前后都通过——包括 `test_a_nonce_cannot_be_reused`（成功后重放**原本**就是 401，本条是「不得回归」的护栏，不是缺陷证据）与 `test_the_sweep_still_runs_for_an_enabled_tenant`（原本行为即正确，同样是防过度修正的护栏）。两条护栏必须前后都绿，这正是它们的作用。

（说明：上面这份失败清单里出现 `test_a_nonce_is_burned_before_the_signature_is_checked`——它在修复前失败是因为**旧实现根本不烧** nonce（`claim_nonce` 位于验签之后），因此「验签前就已占用」这一条断言自然为假；`test_a_request_without_headers_spends_no_nonce` 与 `test_an_unknown_node_spends_no_nonce` 在修复前后都通过（旧实现同样不烧），它们是**边界护栏**，用来钉住「修复不得让未认证请求获得填表能力」。）

### 7.6 同一复现、修复后重跑（与第一/二节的 P 编号逐一对照）

7.5 证明「新测试在修复前会红」；这里把**报告前文记录的那几条复现本身**在修复后的 app 上重跑一遍，逐条对照（`.tmp/fix5_probe5.py`，P401–P405）：

| 报告前文的复现 | 修复前（原文记录） | 修复后（本次实测） |
|---|---|---|
| P119 `expire` 对不存在的租户 | `200 {"expired": 0}` | `404 {"code":"not_found","message":"tenant \`ten_nope\` was not found"}`，且与 `GET /v1/tenants/ten_nope` 的响应体**逐字节相同**（`identical body=True`） |
| P122 `expire` 对**已禁用**租户（含一条已 backdate 的 `approved` 审批） | `200 {"expired": 1}`，`approved`→`expired`，新增 1 条 `approval.expired` 审计 | `404 not_found`；审批仍为 `approved`；审计行数 `4 → 4`（未新增） |
| P82 同一份签名字节：首次被拒 → 操作员推进到 `running` → 重放同样字节 | 首次 `409 invalid_transition`；重放 **`200 succeeded`** | 首次仍 `409 invalid_transition`；重放 **`401 unauthorized`**；任务停在 `running`（未被改写）；nonce 行数 1 |
| P83 签名错误 → 同一 nonce 换正确签名 | `401`；随后 **`200`** | `401 the request signature is not valid for this node`；随后 **`401 unauthorized`**；nonce 行数 1 |
| 正常成功路径（护栏） | `200`，重放 `401` | `200`，重放 `401 this request nonce has already been used`，nonce 行数 1（**未变**） |

两条修复都没有改变成功路径的可观测行为：最后一行修复前后完全相同。P402 中「已禁用租户」与「不存在租户」返回的 `not_found` 消息也**同形**（都走 `require_tenant`，`service.py:104-105`），因此这次修复没有顺带造出一个「租户是否被禁用」的区分器。

### 7.7 本次修复引入的新代价（单列，不当成原有缺陷）

修 2b 把 nonce 占用移到验签**之前**，因此占用不再要求调用方证明持有私钥。把它量出来（`.tmp/fix5_probe6.py`，P501–P505）：

| 量 | 实测 |
|---|---|
| 垃圾签名 + 已知 node id，200 次不同 nonce | **200 行**写入 `node_nonces`，1.40 s（**7.0 ms/请求**，P501） |
| 单个 nonce 长度 | 100 000 字符**原样落库**（`MAX(LENGTH(nonce)) = 100000`，P503）——`String(128)` 在 SQLite 上不强制 |
| 未知 node id（20 次） | **0 行**（P504） |
| 已禁用租户的 node（20 次） | **0 行**（P505） |
| 清理 | 行以 `now` 为戳，600 s 后才可能被清；清理语句不区分「谁的」行（P502） |

即：**任何知道某个 node id 的人**可无凭据地写入行，行宽不受限（受请求体/头大小限制），存活整个签名窗口。node id 不是秘密——P98 已证明同一公钥跨租户复用时可推出/node id 可换，P99 的 WS `ready` 帧直接回显 `nodeId`。

**为什么本次不加护栏**：任务 14 明确只修两条 P0，nonce 长度上限与数据库层护栏属于「明确不做」清单里的相邻项，顺手加会扩大 diff 并需要自己的回归证据。**建议的最小修法**（留给后续）：在 `service.py:248` 占用之前加一条显式长度检查（与 `String(128)` 对齐；超长返回与验签失败**相同**的 `Unauthorized`，不要引入新的可区分消息），并把清理挪到独立事务里按 `at` 批量删；若部署环境是 PostgreSQL，`varchar(128)` 会直接把这条从「100 000 字符落库」变成「驱动层报错 → 500」，所以**长度检查在两种后端下都是必要的**。

一个刻意的设计结论：**没有**把占用改成「验签成功后再补一次独立提交」。那样会恢复「验签失败不烧」的 300 s 爆破窗口（正是 P83 类重放），要挡它反而需要按 `(node_id, 时间窗)` 限速——那是另一个功能，不是修复。

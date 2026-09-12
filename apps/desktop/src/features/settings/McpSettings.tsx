/**
 * MCP 服务器设置（ADR 0014）：列表、配置、启停。
 *
 * ## 这个文件为什么长这样
 *
 * 与 `HostKeySection` 同一个理由：容器（`McpSettings`）负责查询与动作，纯展示组件
 * （`McpSettingsPanel`）只吃数据与回调，于是「一个崩掉的服务器在界面上说了什么」可以用
 * `renderToStaticMarkup` 钉住，而不需要造一个 Tauri 壳和一台真的 MCP 服务器。
 *
 * ## 四条不会让步的规则
 *
 * 1. **传输方式只提供 stdio。** `http` 不是一个「暂时不可用」的选项，而是**在类型层面就不存在**
 *    的东西（`McpStdioConfig` 拒绝它，MCP README「进程生命周期仍然归 Rust」：出站网络策略还没有）。所以这里没有
 *    `http` 选项 —— 摆一个保存时才失败的选项，等于用一个下拉框骗人。
 * 2. **已经存在的 `http` 行要说出来。** 表里可能有旧版本写下的 `http` 行：它照原样显示，
 *    加上后端给出的完整拒绝理由，并且**没有**任何「忽略并继续」的路径。它既不是静默的
 *    no-op，也不是一个能点下去的按钮。
 * 3. **崩了不自愈，界面也不假装会。** 「已退出」这一行会把退出原因与时间摆出来；「启动」是
 *    用户自己的动作，而不是界面替用户重试（ADR 0014：重启可能把上一次的副作用再执行一遍）。
 * 4. **它不会说 MCP 已经连上。** 服务器跑起来、工具注册进 sidecar 都发生在别处；这里的每一
 *    句话都只描述「这台进程现在在不在」。工具数量只有在真的拿到描述符时才显示。
 */

import { useId, useState } from "react";

import { Icon } from "../../components/Icon.js";
import type { McpServerView } from "@yukinal/shared";
import { errorMessage } from "../../lib/format.js";
import {
  draftFromServer,
  emptyDraft,
  mcpExitText,
  mcpUnavailableText,
  previewToolNamespace,
  serverMessages,
  statusLabel,
  toSaveInput,
  useMcpServerActions,
  useMcpServers,
  type McpServerDraft,
} from "../../lib/mcp.js";

export function McpSettings() {
  const serversQuery = useMcpServers();
  const actions = useMcpServerActions();
  // 「正在编辑哪一行」是界面状态：`null` = 没有在编辑。它不是服务端状态，所以不进缓存。
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState<McpServerDraft>(emptyDraft());
  const [notice, setNotice] = useState<string | null>(null);

  const servers = serversQuery.data?.servers ?? [];
  const error =
    (serversQuery.error ? errorMessage(serversQuery.error) : null) ??
    (actions.save.error ? errorMessage(actions.save.error) : null) ??
    (actions.remove.error ? errorMessage(actions.remove.error) : null) ??
    (actions.start.error ? errorMessage(actions.start.error) : null) ??
    (actions.stop.error ? errorMessage(actions.stop.error) : null);

  const beginNew = (): void => {
    setEditing("");
    setDraft(emptyDraft());
    setNotice(null);
  };
  const beginEdit = (view: McpServerView): void => {
    setEditing(view.config.id);
    setDraft(draftFromServer(view.config));
    setNotice(null);
  };
  const submit = (): void => {
    actions.save.mutate(toSaveInput(draft), {
      onSuccess: (view) => {
        setEditing(null);
        setNotice(`已保存 ${view.config.label}（${view.config.id}）。保存不会启动它。`);
      },
    });
  };

  return (
    <McpSettingsPanel
      servers={servers}
      loading={serversQuery.isLoading}
      error={error}
      notice={notice}
      editingId={editing}
      draft={draft}
      busy={actions.busy}
      onDraftChange={setDraft}
      onBeginNew={beginNew}
      onBeginEdit={beginEdit}
      onCancelEdit={() => {
        setEditing(null);
        setNotice(null);
      }}
      onSubmit={submit}
      onStart={(serverId) =>
        actions.start.mutate(serverId, {
          onSuccess: (view) => {
            setNotice(
              view.status.running
                ? `${view.config.label} 已启动（pid ${view.status.pid ?? "?"}）。工具会在 Agent 下次启动时注册。`
                : `${view.config.label} 没有启动成功，原因见列表。`,
            );
          },
        })
      }
      onStop={(serverId) =>
        actions.stop.mutate(serverId, {
          onSuccess: (response) => {
            const shutdown = response.shutdown;
            setNotice(
              shutdown === undefined
                ? `${response.server.config.label} 本来就没有在跑的进程。`
                : shutdown.killed
                  ? `${response.server.config.label} 已关闭（关闭 stdin 无效，已强制结束）。`
                  : `${response.server.config.label} 已关闭。`,
            );
          },
        })
      }
      onDelete={(serverId) =>
        actions.remove.mutate(serverId, {
          onSuccess: (response) => {
            setNotice(
              response.stopped
                ? `${serverId} 已删除，它的进程也一起关掉了。`
                : `${serverId} 已删除（当时没有在跑的进程）。`,
            );
          },
        })
      }
    />
  );
}

/** 表单里能填的字段名 —— 只有这四个，加上开关。 */
type DraftField = "id" | "label" | "command" | "args";

export interface McpSettingsPanelProps {
  servers?: McpServerView[];
  loading?: boolean;
  error?: string | null;
  notice?: string | null;
  /** `null` = 没在编辑；`""` = 在新增（id 还没填）。 */
  editingId?: string | null;
  draft: McpServerDraft;
  busy?: boolean;
  onDraftChange: (draft: McpServerDraft) => void;
  onBeginNew: () => void;
  onBeginEdit: (server: McpServerView) => void;
  onCancelEdit: () => void;
  onSubmit: () => void;
  onStart: (serverId: string) => void;
  onStop: (serverId: string) => void;
  onDelete: (serverId: string) => void;
}

/** 纯展示：所有输入来自 props，可以直接渲染进静态 HTML 来断言。 */
export function McpSettingsPanel({
  servers = [],
  loading = false,
  error = null,
  notice = null,
  editingId = null,
  draft,
  busy = false,
  onDraftChange,
  onBeginNew,
  onBeginEdit,
  onCancelEdit,
  onSubmit,
  onStart,
  onStop,
  onDelete,
}: McpSettingsPanelProps) {
  const titleId = useId();
  const isEditing = editingId !== null;
  const creating = editingId === "";
  const namespace = previewToolNamespace(draft.id);
  const field = (name: DraftField) => `${titleId}-${name}`;

  return (
    <section className="settings-card" aria-labelledby={titleId}>
      <header className="settings-card-header">
        <div>
          <p className="eyebrow">MCP</p>
          <h2 id={titleId}>MCP 服务器</h2>
          <p>
            模型可以调用 MCP 服务器提供的工具。进程由 Yukinal 自己派生与回收，每个服务器的工具
            命名遵守 <code>mcp.&lt;服务器&gt;.&lt;工具&gt;</code>（ADR 0004）。
          </p>
        </div>
        <div className="settings-card-header-actions">
          <button type="button" className="button-secondary" onClick={onBeginNew} disabled={busy}>
            <Icon name="plus" size="sm" /> 添加服务器
          </button>
        </div>
      </header>

      {loading ? <p className="form-hint">正在读取 MCP 服务器…</p> : null}

      {!loading && servers.length === 0 ? (
        <p className="form-hint">
          还没有配置任何 MCP 服务器。添加一个 stdio 服务器（例如 <code>npx -y 某个包</code>）之后，
          它的工具会在 Agent 下次启动时出现在模型可用的工具列表里。
        </p>
      ) : null}

      {servers.length > 0 ? (
        <ul className="provider-list">
          {servers.map((view) => {
            const status = statusLabel(view);
            const unavailable = mcpUnavailableText(view);
            const exit = mcpExitText(view);
            const messages = serverMessages(view);
            return (
              <li key={view.config.id} className="provider-row">
                <div className="provider-copy">
                  <div className="provider-name">
                    <span className={`provider-status-dot ${status.running ? "provider-status-on" : "provider-status-off"}`} />
                    <span>{view.config.label}</span>
                    <span className="settings-status">{status.text}</span>
                    {view.config.enabled ? null : <span className="settings-status">已禁用</span>}
                  </div>
                  <p className="provider-meta">
                    <code>{view.config.id}</code>
                    <span> · {view.config.transport}</span>
                    {view.config.command ? <span> · {view.config.command}</span> : null}
                    {view.status.running ? (
                      <span>
                        {" "}
                        · pid {view.status.pid ?? "?"}
                        {view.status.protocolVersion ? ` · MCP ${view.status.protocolVersion}` : ""}
                      </span>
                    ) : null}
                  </p>
                  {view.status.running ? (
                    <p className="form-hint">
                      {`服务器声明了 ${view.status.toolCount} 个工具`}
                      {view.tools.length === 0
                        ? "，正在读取描述符…"
                        : `：${view.tools.map((tool) => tool.name).join("、")}（这是服务器的拼写；
                           模型看到的名字是 mcp.… 形式，由宿主在注册时规范化）`}
                    </p>
                  ) : null}
                  {/* 崩过与从未启动过是两件不同的事：这一行是「为什么」的出口。 */}
                  {exit ? <p className="form-hint form-hint-warning">{exit}</p> : null}
                  {unavailable ? (
                    <p className="form-error" role="alert">
                      {unavailable}
                    </p>
                  ) : null}
                  {messages.length > 0 ? (
                    <details className="settings-storage-note">
                      <summary>服务器输出（已脱敏、已截断）</summary>
                      {messages.map((line, index) => (
                        <p key={`${view.config.id}-msg-${index}`} className="form-hint">
                          {line}
                        </p>
                      ))}
                    </details>
                  ) : null}
                </div>
                <div className="provider-row-actions">
                  {view.status.running ? (
                    <button
                      type="button"
                      className="button-secondary"
                      onClick={() => onStop(view.config.id)}
                      disabled={busy}
                    >
                      <Icon name="stop" size="sm" /> 停止
                    </button>
                  ) : (
                    // 一个起不来的行不该摆出一个只会失败的按钮：目录与视图已经说明了理由，
                    // 这里就不再重复给一个「点了也没用」的动作。
                    <button
                      type="button"
                      className="button-primary"
                      onClick={() => onStart(view.config.id)}
                      disabled={busy || !view.config.enabled || view.config.transport !== "stdio"}
                      title={
                        view.config.enabled
                          ? view.config.transport === "stdio"
                            ? "启动这个服务器的进程，并读取它的工具列表。"
                            : "这个传输方式目前无法启动：见左边的理由。"
                          : "这一行被禁用了；先在编辑里启用它。"
                      }
                    >
                      启动
                    </button>
                  )}
                  <button
                    type="button"
                    className="button-secondary"
                    onClick={() => onBeginEdit(view)}
                    disabled={busy}
                  >
                    <Icon name="edit" size="sm" /> 编辑
                  </button>
                  <button
                    type="button"
                    className="button-secondary"
                    onClick={() => onDelete(view.config.id)}
                    disabled={busy}
                  >
                    <Icon name="trash" size="sm" /> 删除
                  </button>
                </div>
              </li>
            );
          })}
        </ul>
      ) : null}

      {isEditing ? (
        <form
          className="settings-form-grid settings-form-grid-provider"
          onSubmit={(event) => {
            event.preventDefault();
            onSubmit();
          }}
        >
          <div className="settings-field">
            <label className="field-label" htmlFor={field("id")}>
              服务器 id
            </label>
            <input
              id={field("id")}
              className="form-input form-input-mono"
              value={draft.id}
              readOnly={!creating}
              onChange={(event) => onDraftChange({ ...draft, id: event.target.value })}
            />
            <p className="form-hint">
              这是身份，也是工具名的来源（ADR 0004）。{namespace ? `工具会叫 ${namespace}。` : "必须能变成小写字母与短横线组成的一段。"}
              {creating ? "" : " 已存在的 id 不能改：改了就是另一个服务器。"}
            </p>
          </div>

          <div className="settings-field">
            <label className="field-label" htmlFor={field("label")}>
              名字
            </label>
            <input
              id={field("label")}
              className="form-input"
              value={draft.label}
              onChange={(event) => onDraftChange({ ...draft, label: event.target.value })}
            />
            <p className="form-hint">只用于在列表里分辨，不影响工具名。</p>
          </div>

          <div className="settings-field">
            <label className="field-label" htmlFor={field("command")}>
              启动命令
            </label>
            <input
              id={field("command")}
              className="form-input form-input-mono"
              value={draft.command}
              onChange={(event) => onDraftChange({ ...draft, command: event.target.value })}
              placeholder="npx"
            />
            <p className="form-hint">
              传输方式固定为 <strong>stdio</strong>：Yukinal 派生这个进程，用标准输入输出与它说话。
              目前<strong>不提供 http 传输</strong> —— 那需要出站网络策略，而它还不存在（MCP README「进程生命周期仍然归 Rust」），
              所以这里没有这个选项，而不是留一个保存时才失败的选项。
            </p>
          </div>

          <div className="settings-field settings-fact-wide">
            <label className="field-label" htmlFor={field("args")}>
              参数（一行一个）
            </label>
            <textarea
              id={field("args")}
              className="form-textarea"
              rows={3}
              value={draft.args}
              onChange={(event) => onDraftChange({ ...draft, args: event.target.value })}
            />
            <p className="form-hint">
              一行一个参数，不做 shell 解析：这里的换行就是一个参数列表，不会被展开成命令拼接。
            </p>
          </div>

          <div className="settings-field">
            <label className="settings-check-row" htmlFor={`${titleId}-enabled`}>
              <input
                id={`${titleId}-enabled`}
                type="checkbox"
                checked={draft.enabled}
                onChange={(event) => onDraftChange({ ...draft, enabled: event.target.checked })}
              />
              <span>
                <strong>启用</strong>
                <small>禁用时保存可以，启动与注册都不行。</small>
              </span>
            </label>
          </div>

          <div className="settings-actions">
            <button type="submit" className="button-primary" disabled={busy}>
              保存
            </button>
            <button type="button" className="button-secondary" onClick={onCancelEdit} disabled={busy}>
              取消
            </button>
          </div>

          <p className="form-hint">
            保存<strong>不会启动</strong>服务器：那是「启动」按钮的事。每个 MCP 工具调用都需要用户逐项批准，
            所以保存一个服务器不等于信任它。
          </p>
        </form>
      ) : null}

      {error ? (
        <p className="form-error" role="alert">
          {error}
        </p>
      ) : null}
      {notice ? (
        <p className="form-success" role="status">
          {notice}
        </p>
      ) : null}
    </section>
  );
}

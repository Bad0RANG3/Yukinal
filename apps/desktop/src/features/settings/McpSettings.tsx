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
 * 1. **传输方式明确选择，字段跟着传输切换。** stdio 只展示 command/args；Streamable HTTP
 *    只展示 endpoint。界面不自行判定 URL 是否安全，Rust 边界会拒绝远程明文 HTTP。
 * 2. **端点不可用时说完整理由。** 无效 URL、缺 command 或网络失败都由后端给出同一句可读
 *    理由；界面没有「忽略并继续」路径。
 * 3. **崩了不自愈，界面也不假装会。** 「已退出」这一行会把退出原因与时间摆出来；「启动」是
 *    用户自己的动作，而不是界面替用户重试（ADR 0014：重启可能把上一次的副作用再执行一遍）。
 * 4. **它不会说 MCP 已经连上。** 服务器跑起来、工具注册进 sidecar 都发生在别处；这里的每一
 *    句话都只描述「这台进程现在在不在」。工具数量只有在真的拿到描述符时才显示。
 */

import { useEffect, useId, useRef, useState } from "react";

import { Icon } from "../../components/Icon.js";
import type { McpOAuthDeviceCodeEvent, McpServerView } from "@yukinal/shared";
import { errorMessage } from "../../lib/format.js";
import { openExternalUrl } from "../../lib/external.js";
import { subscribeDesktop } from "../../lib/ipc.js";
import {
  draftFromServer,
  emptyDraft,
  mcpExitText,
  mcpRestartText,
  mcpUnavailableText,
  oauthClientAuthNeedsSecret,
  serverMessages,
  statusLabel,
  toSaveInput,
  useMcpServerActions,
  useMcpServers,
  type McpServerDraft,
} from "../../lib/mcp.js";
import {
  McpOAuthDeviceCodeDialog,
  type McpOAuthDeviceCodePhase,
} from "./McpOAuthDeviceCodeDialog.js";

export function McpSettings({ active = true }: { active?: boolean } = {}) {
  const serversQuery = useMcpServers({ enabled: active });
  const actions = useMcpServerActions();
  // 「正在编辑哪一行」是界面状态：`null` = 没有在编辑。它不是服务端状态，所以不进缓存。
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState<McpServerDraft>(emptyDraft());
  const [notice, setNotice] = useState<string | null>(null);
  // 设备码流程：Rust 在**等待期间**把 user_code 作为事件发出来，因为那个请求还没有返回。
  // 这里只保存「屏幕该显示什么」与用户自己的两个动作（复制、取消），没有令牌。
  const [devicePrompt, setDevicePrompt] = useState<McpOAuthDeviceCodeEvent | null>(null);
  const [cancelled, setCancelled] = useState(false);
  const [copyHint, setCopyHint] = useState<string | null>(null);
  const [linkError, setLinkError] = useState<string | null>(null);

  const servers = serversQuery.data?.servers ?? [];
  const error =
    (serversQuery.error ? errorMessage(serversQuery.error) : null) ??
    (actions.save.error ? errorMessage(actions.save.error) : null) ??
    (actions.remove.error ? errorMessage(actions.remove.error) : null) ??
    (actions.start.error ? errorMessage(actions.start.error) : null) ??
    (actions.stop.error ? errorMessage(actions.stop.error) : null) ??
    (actions.oauthCancel.error ? errorMessage(actions.oauthCancel.error) : null) ??
    // 设备码流程的终态由对话框说 —— 用户正看着那个代码；对话框关掉之后，这句话留在这里。
    (devicePrompt === null && actions.oauthConnect.error
      ? errorMessage(actions.oauthConnect.error)
      : null);

  useEffect(() => {
    const subscription = subscribeDesktop("mcp.oauth_device_code", (prompt) => {
      setDevicePrompt(prompt);
      setCancelled(false);
      setCopyHint(null);
      setLinkError(null);
    });
    return () => subscription.stop();
  }, []);

  const dismissDevicePrompt = (): void => {
    setDevicePrompt(null);
    setCancelled(false);
    setCopyHint(null);
    setLinkError(null);
  };

  const copyDeviceCode = (): void => {
    if (!devicePrompt) return;
    // 代码是给用户看的，不是机密：复制按钮只是省一次手抄。剪贴板不可用时如实说明，
    // 因为屏幕上的代码始终还在，用户仍然有办法。
    navigator.clipboard?.writeText(devicePrompt.userCode).then(
      () => setCopyHint("已复制到剪贴板。"),
      () => setCopyHint("这个环境不允许写剪贴板，请手动输入上面的代码。"),
    );
  };

  const openDeviceLink = (url: string): void => {
    setLinkError(null);
    void openExternalUrl(url).catch((cause: unknown) =>
      setLinkError(cause instanceof Error ? cause.message : String(cause)),
    );
  };

  const devicePhase: McpOAuthDeviceCodePhase = actions.oauthConnect.isPending
    ? "waiting"
    : cancelled
      ? "cancelled"
      : actions.oauthConnect.isError
        ? "failed"
        : "succeeded";
  const deviceMessage = cancelled
    ? "已取消这次授权；需要时重新点「连接 OAuth」会得到一个新的代码。"
    : linkError
      ? `无法打开链接：${linkError}`
      : actions.oauthConnect.error
        ? errorMessage(actions.oauthConnect.error)
        : null;

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
    <>
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
                  ? `${view.config.label} 已启动（pid ${view.status.pid ?? "?"}），重启运行服务后生效。`
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
        onReview={(input, label) =>
          actions.review.mutate(input, {
            onSuccess: () =>
              setNotice(
                `${label} 的工具审核已保存，重启运行服务后生效。`,
              ),
          })
        }
        onOAuthConnect={(serverId) => {
          // 新的一次授权不能沿用上一次留下的对话框状态（代码、取消标记、复制提示）。
          dismissDevicePrompt();
          actions.oauthConnect.mutate(serverId, {
            onSuccess: (response) =>
              setNotice(
                `${response.serverId} 已完成 OAuth 授权；运行中的旧会话已关闭，下次启动会使用新令牌。`,
              ),
          });
        }}
      />
      {devicePrompt ? (
        <McpOAuthDeviceCodeDialog
          prompt={devicePrompt}
          phase={devicePhase}
          message={deviceMessage}
          copyHint={copyHint}
          onOpenLink={openDeviceLink}
          onCopyCode={copyDeviceCode}
          onCancel={() => {
            // 界面先记住「是用户自己要停的」：后端随后报回来的那句失败是同一件事。
            setCancelled(true);
            setCopyHint(null);
            actions.oauthCancel.mutate(devicePrompt.serverId);
          }}
          onDismiss={dismissDevicePrompt}
        />
      ) : null}
    </>
  );
}

/** 表单里能填的字段名。 */
type DraftField =
  | "id"
  | "label"
  | "transport"
  | "command"
  | "args"
  | "url"
  | "authMode"
  | "httpAuthHeaders"
  | "oauthIssuer"
  | "oauthClientId"
  | "oauthFlow"
  | "oauthClientAuth"
  | "oauthClientSecret"
  | "oauthDpop"
  | "oauthScopes";

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
  onReview: (
    input: {
      serverId: string;
      allowedTools: string[];
      trustLevel: "reviewed" | "unreviewed";
    },
    label: string,
  ) => void;
  onOAuthConnect: (serverId: string) => void;
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
  onReview,
  onOAuthConnect,
}: McpSettingsPanelProps) {
  const titleId = useId();
  const isEditing = editingId !== null;
  const creating = editingId === "";
  const field = (name: DraftField) => `${titleId}-${name}`;
  const editorTrigger = useRef<HTMLButtonElement | null>(null);
  const editorForm = useRef<HTMLFormElement>(null);
  const previousEditing = useRef<string | null>(null);
  useEffect(() => {
    if (editingId !== null) editorForm.current?.querySelector<HTMLInputElement>("input:not([readonly])")?.focus();
    else if (previousEditing.current !== null) editorTrigger.current?.focus();
    previousEditing.current = editingId;
  }, [editingId]);

  return (
    <section className="settings-card" aria-labelledby={titleId}>
      <header className="settings-card-header">
        <div>
          <h2 id={titleId}>MCP 服务器</h2>
        </div>
        <div className="settings-card-header-actions">
          <button type="button" className="button-secondary" onClick={(event) => { editorTrigger.current = event.currentTarget; onBeginNew(); }} disabled={busy}>
            <Icon name="plus" size="sm" /> 添加服务器
          </button>
        </div>
      </header>

      {loading ? <p className="form-hint">正在读取 MCP 服务器…</p> : null}

      {!loading && servers.length === 0 ? <p className="muted-copy">暂无 MCP 服务器</p> : null}

      {servers.length > 0 ? (
        <ul className="provider-list">
          {servers.map((view) => {
            const status = statusLabel(view);
            const unavailable = mcpUnavailableText(view);
            const exit = mcpExitText(view);
            const restart = mcpRestartText(view);
            const messages = serverMessages(view);
            const configurationBlocksStart =
              view.unavailable?.code === "invalid_config" ||
              view.unavailable?.code === "disabled";
            return (
              <li key={view.config.id} className="provider-row">
                <div className="provider-copy">
                  <div className="provider-name">
                    <span className={`provider-status-dot ${status.running ? "provider-status-on" : "provider-status-off"}`} />
                    <span className="provider-label">{view.config.label}</span>
                    <span className="settings-status">{status.text}</span>
                    {view.config.enabled ? null : <span className="settings-status">已禁用</span>}
                  </div>
                  <p className="provider-meta">
                    <code>{view.config.id}</code>
                    <span> · {view.config.transport}</span>
                    {view.config.command ? <span> · {view.config.command}</span> : null}
                    {view.config.url ? <span> · {view.config.url}</span> : null}
                    {view.config.oauth ? (
                      <span>
                        {" "}
                        · OAuth{" "}
                        {view.config.oauth.flow === "device_code" ? "设备码" : "授权码"} ·{" "}
                        {view.config.oauth.clientAuth === "none"
                          ? ""
                          : `${view.config.oauth.clientAuth} · `}
                        {view.config.oauth.dpop ? "DPoP · " : ""}
                        {view.config.oauth.issuer || "（连接时自动发现）"}
                      </span>
                    ) : view.config.httpAuthHeaders.length > 0 ? (
                      <span>
                        {" "}
                        · auth {view.config.httpAuthHeaders.map((header) => header.name).join(", ")}
                      </span>
                    ) : null}
                    {view.status.running && view.status.pid != null ? (
                      <span>
                        {" "}
                        · pid {view.status.pid}
                        {view.status.protocolVersion ? ` · MCP ${view.status.protocolVersion}` : ""}
                      </span>
                    ) : null}
                    {view.status.running && view.status.pid == null && view.status.protocolVersion ? (
                      <span> · MCP {view.status.protocolVersion}</span>
                    ) : null}
                  </p>
                  {view.status.running ? (
                    <p className="form-hint">
                      {`工具 ${view.status.toolCount}`}
                    </p>
                  ) : null}
                  {view.status.running && view.tools.length > 0 ? (
                    <McpToolReview
                      key={`${view.config.id}:${view.tools.map((tool) => tool.name).join(",")}`}
                      view={view}
                      busy={busy}
                      onSave={(input) => onReview(input, view.config.label)}
                    />
                  ) : null}
                  {/* 崩过与从未启动过是两件不同的事：这一行是「为什么」的出口。 */}
                  {exit ? <p className="form-hint form-hint-warning">{exit}</p> : null}
                  {restart ? <p className="form-hint form-hint-warning">{restart}</p> : null}
                  {unavailable ? (
                    <p className="form-error" role="alert">
                      {unavailable}
                    </p>
                  ) : null}
                  {messages.length > 0 ? (
                    <details className="settings-storage-note">
                      <summary>服务器输出</summary>
                      {messages.map((line, index) => (
                        <p key={`${view.config.id}-msg-${index}`} className="form-hint">
                          {line}
                        </p>
                      ))}
                    </details>
                  ) : null}
                </div>
                <div className="provider-row-actions">
                  {view.config.oauth ? (
                    <button
                      type="button"
                      className="button-secondary"
                      onClick={() => onOAuthConnect(view.config.id)}
                      disabled={busy}
                      title="打开系统浏览器完成 OAuth 授权；成功后旧的运行会话会关闭。"
                    >
                      <Icon name="connect" size="sm" />{" "}
                      {view.config.oauth.credentialRef ? "重新授权" : "连接 OAuth"}
                    </button>
                  ) : null}
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
                      disabled={busy || !view.config.enabled || configurationBlocksStart}
                      title={
                        configurationBlocksStart
                          ? "先修正左边的配置理由，再启动。"
                          : view.config.enabled
                          ? view.config.transport === "stdio"
                            ? "启动这个服务器的进程，并读取它的工具列表。"
                            : "连接 HTTP endpoint，并读取它的工具列表。"
                          : "这一行被禁用了；先在编辑里启用它。"
                      }
                    >
                      启动
                    </button>
                  )}
                  <button
                    type="button"
                    className="button-secondary"
                    onClick={(event) => { editorTrigger.current = event.currentTarget; onBeginEdit(view); }}
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
          ref={editorForm}
          className="settings-form-grid settings-form-grid-provider mcp-settings-form"
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
          </div>

          <div className="settings-field">
            <label className="field-label" htmlFor={field("transport")}>
              传输方式
            </label>
            <select
              id={field("transport")}
              className="form-input"
              value={draft.transport}
              onChange={(event) => onDraftChange({ ...draft, transport: event.target.value })}
            >
              <option value="stdio">stdio（本机子进程）</option>
              <option value="http">Streamable HTTP</option>
            </select>
          </div>

          {draft.transport === "stdio" ? (
            <>
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
              </div>

              <div className="settings-field settings-fact-wide">
                <label className="field-label" htmlFor={field("args")}>
                  参数（一行一个）
                </label>
                <textarea
                  id={field("args")}
                  className="form-input form-textarea"
                  rows={3}
                  value={draft.args}
                  onChange={(event) => onDraftChange({ ...draft, args: event.target.value })}
                />
              </div>
            </>
          ) : draft.transport === "http" ? (
            <>
              <div className="settings-field settings-fact-wide">
                <label className="field-label" htmlFor={field("url")}>
                  Endpoint URL
                </label>
                <input
                  id={field("url")}
                  className="form-input form-input-mono"
                  value={draft.url}
                  onChange={(event) => onDraftChange({ ...draft, url: event.target.value })}
                  placeholder="https://mcp.example.com/mcp"
                  spellCheck={false}
                />
              </div>

              <div className="settings-field">
                <label className="field-label" htmlFor={field("authMode")}>
                  Authentication
                </label>
                <select
                  id={field("authMode")}
                  className="form-input"
                  value={draft.authMode}
                  onChange={(event) =>
                    onDraftChange({
                      ...draft,
                      authMode: event.target.value as McpServerDraft["authMode"],
                    })
                  }
                >
                  <option value="none">无认证</option>
                  <option value="static">静态请求头</option>
                  <option value="oauth">OAuth 2.1</option>
                </select>
              </div>

              {draft.authMode === "static" ? (
                <div className="settings-field settings-fact-wide">
                  <label className="field-label" htmlFor={field("httpAuthHeaders")}>
                    Authentication headers（一行一个）
                  </label>
                  <textarea
                    id={field("httpAuthHeaders")}
                    className="form-input form-textarea form-input-mono"
                    rows={4}
                    value={draft.httpAuthHeaders}
                    onChange={(event) =>
                      onDraftChange({ ...draft, httpAuthHeaders: event.target.value })
                    }
                    placeholder={"Authorization: Bearer …\nX-API-Key: …"}
                    spellCheck={false}
                  />
                </div>
              ) : null}

                  {draft.authMode === "oauth" ? (
                    <>
                      <div className="settings-field">
                        <label className="field-label" htmlFor={field("oauthClientAuth")}>
                          Client authentication
                        </label>
                        <select
                          id={field("oauthClientAuth")}
                          className="form-input"
                          value={draft.oauthClientAuth}
                          onChange={(event) =>
                            onDraftChange({
                              ...draft,
                              oauthClientAuth: event.target
                                .value as McpServerDraft["oauthClientAuth"],
                              // 换一种认证方式就换了一份凭据，输入框里不该还留着上一种的。
                              oauthClientSecret: "",
                            })
                          }
                        >
                          <option value="none">公共客户端（只发送 client id）</option>
                          <option value="client_secret_post">client_secret_post</option>
                          <option value="client_secret_basic">client_secret_basic</option>
                        </select>
                      </div>
                      {oauthClientAuthNeedsSecret(draft.oauthClientAuth) ? (
                        <div className="settings-field">
                          <label className="field-label" htmlFor={field("oauthClientSecret")}>
                            Client secret
                          </label>
                          <input
                            id={field("oauthClientSecret")}
                            className="form-input form-input-mono"
                            type="password"
                            value={draft.oauthClientSecret}
                            autoComplete="off"
                            spellCheck={false}
                            placeholder="留空保留现有 secret"
                            onChange={(event) =>
                              onDraftChange({ ...draft, oauthClientSecret: event.target.value })
                            }
                          />
                        </div>
                      ) : null}
                      <div className="settings-field">
                        <label className="field-label" htmlFor={field("oauthIssuer")}>
                      OAuth issuer
                    </label>
                    <input
                      id={field("oauthIssuer")}
                      className="form-input form-input-mono"
                      value={draft.oauthIssuer}
                      onChange={(event) =>
                        onDraftChange({ ...draft, oauthIssuer: event.target.value })
                      }
                      placeholder="https://auth.example.com"
                      spellCheck={false}
                    />
                  </div>
                  <div className="settings-field">
                    <label className="field-label" htmlFor={field("oauthClientId")}>
                      OAuth client id
                    </label>
                    <input
                      id={field("oauthClientId")}
                      className="form-input form-input-mono"
                      value={draft.oauthClientId}
                      onChange={(event) =>
                        onDraftChange({ ...draft, oauthClientId: event.target.value })
                      }
                      placeholder="留空则使用动态客户端注册"
                      spellCheck={false}
                    />
                  </div>
                  <div className="settings-field settings-fact-wide">
                    <label className="field-label" htmlFor={field("oauthFlow")}>
                      OAuth 流程
                    </label>
                    <select
                      id={field("oauthFlow")}
                      className="form-input"
                      value={draft.oauthFlow}
                      onChange={(event) =>
                        onDraftChange({
                          ...draft,
                          oauthFlow: event.target.value as McpServerDraft["oauthFlow"],
                        })
                      }
                    >
                      <option value="authorization_code">
                        Authorization Code + PKCE（浏览器回调）
                      </option>
                      <option value="device_code">Device Code（设备码，RFC 8628）</option>
                    </select>
                  </div>
                  <div className="settings-field settings-fact-wide">
                    <label className="settings-check-row" htmlFor={field("oauthDpop")}>
                      <input
                        id={field("oauthDpop")}
                        type="checkbox"
                        checked={draft.oauthDpop}
                        onChange={(event) =>
                          onDraftChange({ ...draft, oauthDpop: event.target.checked })
                        }
                      />
                      <span>
                        <strong>发送方约束令牌（DPoP）</strong>
                      </span>
                    </label>
                  </div>
                  <div className="settings-field settings-fact-wide">
                    <label className="field-label" htmlFor={field("oauthScopes")}>
                      Scopes（空白或换行分隔）
                    </label>
                    <textarea
                      id={field("oauthScopes")}
                      className="form-input form-textarea form-input-mono"
                      rows={3}
                      value={draft.oauthScopes}
                      onChange={(event) =>
                        onDraftChange({ ...draft, oauthScopes: event.target.value })
                      }
                      placeholder={"mcp.read\nmcp.tools"}
                      spellCheck={false}
                    />
                  </div>
                </>
              ) : null}
            </>
          ) : null}

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
              </span>
            </label>
          </div>

          <div className="settings-actions field-wide">
            <button type="submit" className="button-primary" disabled={busy}>
              保存
            </button>
            <button type="button" className="button-secondary" onClick={onCancelEdit} disabled={busy}>
              取消
            </button>
          </div>


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

function McpToolReview({
  view,
  busy,
  onSave,
}: {
  view: McpServerView;
  busy: boolean;
  onSave: (input: {
    serverId: string;
    allowedTools: string[];
    trustLevel: "reviewed" | "unreviewed";
    annotationTrust: "none" | "trusted";
  }) => void;
}) {
  const [selected, setSelected] = useState<string[]>(view.config.allowedTools);
  const [reviewed, setReviewed] = useState(view.config.trustLevel === "reviewed");
  const [trustAnnotations, setTrustAnnotations] = useState(
    view.config.annotationTrust === "trusted",
  );
  return (
    <details className="settings-storage-note">
      <summary>审核工具（当前允许 {view.config.allowedTools.length} 个）</summary>
      <div className="mcp-tool-review">
        {view.tools.map((tool) => (
          <label className="settings-check-row" key={tool.name}>
            <input
              type="checkbox"
              checked={selected.includes(tool.name)}
              onChange={(event) =>
                setSelected((current) =>
                  event.target.checked
                    ? [...current, tool.name]
                    : current.filter((name) => name !== tool.name),
                )
              }
            />
            <span>
              <strong>{tool.name}</strong>
            </span>
          </label>
        ))}
        <label className="settings-check-row">
          <input type="checkbox" checked={reviewed} onChange={(event) => setReviewed(event.target.checked)} />
          <span>
            <strong>已完成审核</strong>
          </span>
        </label>
        <label className="settings-check-row">
          <input
            type="checkbox"
            checked={trustAnnotations}
            onChange={(event) => setTrustAnnotations(event.target.checked)}
          />
          <span>
            <strong>信任此服务器声明的只读/破坏性注解</strong>
          </span>
        </label>
        <button
          type="button"
          className="button-secondary"
          disabled={busy}
          onClick={() =>
            onSave({
              serverId: view.config.id,
              allowedTools: selected,
              trustLevel: reviewed ? "reviewed" : "unreviewed",
              annotationTrust: trustAnnotations ? "trusted" : "none",
            })
          }
        >
          保存工具审核
        </button>
      </div>
    </details>
  );
}

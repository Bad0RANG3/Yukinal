import { useIsMutating, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { HttpBaseUrlSchema, IPC_COMMANDS, type AiProviderConfig } from "@yukinal/shared";
import { useId, useState, type ReactNode } from "react";

import { Icon } from "../../components/Icon.js";
import { KeywordText } from "../../components/KeywordText.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { TERMINAL_FONT_LABEL, TERMINAL_FONT_ORDER } from "../../lib/labels.js";
import { PROVIDERS_QUERY_KEY, useProviders } from "../../lib/providers.js";
import { useAgentLogs, useAgentStatus, useCorePing } from "../../lib/runtime.js";
import { usePresence } from "../../hooks/usePresence.js";
import {
  usePreferencesStore,
  type TerminalFont,
  type TerminalFontSize,
  type TerminalLineHeight,
  type UiDensity,
  type UiFontSize,
} from "../../stores/preferences-store.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";

export function RuntimeSettings() {
  const [showLogs, setShowLogs] = useState(false);
  const core = useCorePing();
  const status = useAgentStatus();
  const logs = useAgentLogs(showLogs);
  const shell = isDesktopShell();

  return (
    <section className="settings-page">
      <div className="settings-page-heading">
        <p className="eyebrow">系统设置</p>
        <h2>运行时与 AI</h2>
        <p>管理本地 Runtime、Provider 和 Yukinal 的外观与交互。</p>
      </div>

      {!shell ? (
        <div className="settings-notice" role="status">
          <Icon name="warning" size="md" />
          <span>浏览器预览：原生 Runtime 调用不可用，请启动 Tauri 桌面壳以连接 Agent。</span>
        </div>
      ) : null}

      <section className="settings-card">
        <div className="settings-card-header">
          <div>
            <p className="eyebrow">Runtime</p>
            <h2>运行时状态</h2>
          </div>
          <span className={`settings-status ${status.data?.running ? "settings-status-ready" : ""}`}>
            {!shell ? "预览模式" : status.isError ? "状态读取失败" : status.isLoading ? "查询中…" : status.data?.running ? "Agent 运行中" : "Agent 未启动"}
          </span>
        </div>
        <dl className="settings-facts">
          <Row label="Core" value={core.data ? `${core.data.version} (${core.data.os})` : unknown(shell)} />
          <Row label="Agent" value={status.data ? (status.data.running ? "运行中" : "已停止") : unknown(shell)} />
          <Row label="Agent PID" value={text(status.data?.pid, shell)} />
          <Row label="Protocol" value={text(status.data?.protocolVersion, shell)} />
          <Row label="Agent version" value={text(status.data?.agentVersion, shell)} />
          <Row label="Tools" value={text(status.data?.toolCount, shell)} />
          <Row label="Sidecar entry" value={text(status.data?.entry, shell)} wide />
          <Row label="Last exit" value={status.data?.lastExit ? `${status.data.lastExit.code ?? status.data.lastExit.signal ?? "未知"} @ ${status.data.lastExit.at}` : "无"} wide />
        </dl>
        <div className="settings-card-actions">
          <button type="button" className="button-secondary" disabled={!shell} aria-expanded={showLogs} aria-controls="runtime-logs" onClick={() => setShowLogs((current) => !current)}>
            <Icon name={showLogs ? "chevronUp" : "logs"} size="sm" />
            {showLogs ? "隐藏 Agent 日志" : "查看 Agent 日志"}
          </button>
        </div>
        {status.isError || core.isError ? <div className="settings-error-row" role="alert"><span>{status.error?.message ?? core.error?.message}</span><button type="button" className="text-button" onClick={() => { void core.refetch(); void status.refetch(); }}>重试</button></div> : null}
        <RuntimeLogs open={showLogs} logs={logs} />
      </section>
          {/*
            自动恢复是一句事实，不能只靠一闪而过的通知：崩溃时用户可能正在别的页面，
            回到设置里必须能查到「它自己起来过几次、还是已经放弃了」。
          */}
          <Row
            label="自动恢复"
            value={
              status.data?.restart
                ? status.data.restart.exhausted
                  ? `已尝试 ${status.data.restart.attempt}/${status.data.restart.maxAttempts} 次仍未成功，不会再次自动尝试`
                  : `崩溃后已自动重启 ${status.data.restart.attempt}/${status.data.restart.maxAttempts} 次`
                : "无"
            }
            wide
          />

      <AppearanceSettings />
      <ProviderSettings />
    </section>
  );
}

/**
 * 「查看 Agent 日志」展开的那块输出。
 *
 * 从 RuntimeSettings 里拆出来有两个理由：一是它有三种互斥的形态（加载中 /
 * 读取失败 / 有内容），挤在一行里既读不了也改不动；二是它需要一段收起动画，
 * 而 presence 要求把 `is-closing` 和 `onAnimationEnd` 交给**当前真正渲染出来的
 * 那个元素** —— 内联写在三元表达式里做不到这件事。
 */
function RuntimeLogs({ open, logs }: { open: boolean; logs: ReturnType<typeof useAgentLogs> }) {
  const presence = usePresence(open, { exitAnimation: "expand-exit" });
  if (!presence.mounted) return null;
  const closing = presence.closing ? " is-closing" : "";
  if (logs.isLoading) return <pre id="runtime-logs" className={`agent-log-viewer${closing}`} onAnimationEnd={presence.onAnimationEnd}>正在读取日志…</pre>;
  if (logs.isError) {
    return (
      <div className={`settings-error-row${closing}`} role="alert" onAnimationEnd={presence.onAnimationEnd}>
        <span>{logs.error.message}</span>
        <button type="button" className="text-button" onClick={() => void logs.refetch()}>重试</button>
      </div>
    );
  }
  return (
    <pre id="runtime-logs" className={`agent-log-viewer${closing}`} onAnimationEnd={presence.onAnimationEnd}>
      {logs.data?.lines.length ? logs.data.lines.join("\n") : "（暂无捕获输出）"}
    </pre>
  );
}

function AppearanceSettings() {
  const preferences = usePreferencesStore();
  const setPreferences = preferences.setPreferences;

  return (
    <section className="settings-card">
      <div className="settings-card-header">
        <div>
          <p className="eyebrow">偏好</p>
          <h2>外观与交互</h2>
          <p>修改会立即应用，并保存在当前设备的本地存储中。</p>
        </div>
        <Icon name="settings" size="lg" />
      </div>
      <div className="settings-form-grid">
        <Field label="UI 字号">
          <select className="form-input" value={preferences.uiFontSize} onChange={(event) => setPreferences({ uiFontSize: Number(event.target.value) as UiFontSize })}>
            <option value={12}>小 · 12px</option>
            <option value={13}>标准 · 13px</option>
            <option value={14}>大 · 14px</option>
          </select>
        </Field>
        <Field label="内容密度">
          <select className="form-input" value={preferences.density} onChange={(event) => setPreferences({ density: event.target.value as UiDensity })}>
            <option value="compact">紧凑</option>
            <option value="comfortable">舒适</option>
          </select>
        </Field>
        <Field label="终端字体">
          <select className="form-input" value={preferences.terminalFont} onChange={(event) => setPreferences({ terminalFont: event.target.value as TerminalFont })}>
            {TERMINAL_FONT_ORDER.map((font) => <option key={font} value={font}>{TERMINAL_FONT_LABEL[font]}</option>)}
          </select>
        </Field>
        <Field label="终端字号">
          <select className="form-input" value={preferences.terminalFontSize} onChange={(event) => setPreferences({ terminalFontSize: Number(event.target.value) as TerminalFontSize })}>
            {[12, 13, 14, 15, 16].map((size) => <option key={size} value={size}>{size}px</option>)}
          </select>
        </Field>
        <Field label="终端行高">
          <select className="form-input" value={preferences.terminalLineHeight} onChange={(event) => setPreferences({ terminalLineHeight: Number(event.target.value) as TerminalLineHeight })}>
            <option value={1.2}>紧凑 · 1.2</option>
            <option value={1.35}>标准 · 1.35</option>
            <option value={1.5}>宽松 · 1.5</option>
          </select>
        </Field>
        <Field label="Agent 权限">
          <select className="form-input" value={preferences.agentPermissionMode} onChange={(event) => setPreferences({ agentPermissionMode: event.target.value as "ask" | "auto" })}>
            <option value="ask">操作前询问（推荐）</option>
            <option value="auto">委托 Agent 自动批准 开发与预发布写入</option>
          </select>
        </Field>
      </div>
      <div className="settings-check-grid">
        <label className="settings-check-row">
          <input type="checkbox" checked={preferences.terminalCursorBlink} onChange={(event) => setPreferences({ terminalCursorBlink: event.target.checked })} />
          <span><strong>光标闪烁</strong><small>终端光标使用闪烁提示</small></span>
        </label>
        <label className="settings-check-row">
          <input type="checkbox" checked={preferences.reduceMotion} onChange={(event) => setPreferences({ reduceMotion: event.target.checked })} />
          <span><strong>减少动效</strong><small>降低过渡和状态动画</small></span>
        </label>
      </div>
      <p className="form-hint">自动批准只适用于开发或预发布目标上的普通写入。本机、未知、生产、高危和 critical 操作仍需逐项确认；策略禁止的操作始终拒绝，允许的自动执行会以“Agent 自主批准”写入审计。</p>
      <div className="settings-actions">
        <button type="button" className="button-secondary" onClick={preferences.resetPreferences}>
          <Icon name="refresh" size="sm" />恢复默认设置
        </button>
        <span className="settings-storage-note">仅保存界面偏好，不保存 API Key 或密码</span>
      </div>
    </section>
  );
}

function ProviderSettings() {
  const queryClient = useQueryClient();
  const shell = isDesktopShell();
  const busy = useIsMutating({ mutationKey: ["provider-write"] }) > 0;
  const selectedProviderId = useWorkspaceStore((state) => state.selectedProviderId);
  const selectProvider = useWorkspaceStore((state) => state.selectProvider);
  // `undefined` follows the active provider, `null` starts a new configuration,
  // and an id keeps a specific saved provider open for editing.
  const [editorProviderId, setEditorProviderId] = useState<string | null | undefined>(undefined);
  const providers = useProviders();
  const selectedProvider = providers.data?.find((provider) => provider.id === selectedProviderId)
    ?? providers.data?.find((provider) => provider.enabled);
  const editorProvider = editorProviderId === undefined
    ? selectedProvider
    : editorProviderId === null
      ? undefined
      : providers.data?.find((provider) => provider.id === editorProviderId);
  const onSaved = ({ provider }: { provider: AiProviderConfig }) => {
    selectProvider(provider.id, provider.model);
    setEditorProviderId(provider.id);
    // 用常量而不是 `["providers"]` 字面量：`servers.ts` 的注释里记过这条教训 ——
    // invalidateQueries 的 key 拼错**不会报任何错**，只是静默刷新不到东西。
    // 这里原来就是那个字面量，而 `PROVIDERS_QUERY_KEY` 早已导出、别处已在用。
    void queryClient.invalidateQueries({ queryKey: PROVIDERS_QUERY_KEY });
  };
  const activate = useMutation({
    mutationKey: ["provider-write"],
    mutationFn: (providerId: string) => callDesktop(IPC_COMMANDS.providerActivate, { providerId }), onSuccess: onSaved,
  });
  return (
    <div className="settings-stack">
      <section className="settings-card">
        <div className="settings-card-header">
          <div><p className="eyebrow">AI</p><h2>Provider</h2><p>新建 Agent 运行会使用当前启用的 Provider。</p></div>
          <div className="settings-card-header-actions"><span className="settings-count">{providers.data?.length ?? 0} 个已配置</span><button type="button" disabled={busy} onClick={() => setEditorProviderId(null)} className="button-secondary button-small">添加 Provider</button></div>
        </div>
        {providers.isLoading ? <p className="muted-copy" role="status">正在加载 Provider…</p> : providers.isError ? <p className="form-error" role="alert">{providers.error.message}</p> : providers.data?.length ? (
          <div className="provider-list">
            {providers.data.map((provider) => (
              <div key={provider.id} className={`provider-row ${provider.enabled ? "provider-row-active" : ""}`}>
                <div className="provider-copy">
                  <div className="provider-name"><span className={`provider-status-dot ${provider.enabled ? "provider-status-on" : "provider-status-off"}`} /><span>{provider.label}</span>{provider.enabled ? <span className="status-badge status-badge-success">当前使用</span> : null}</div>
                  <div className="provider-meta"><span><KeywordText text={provider.model} /></span><span>·</span><span><KeywordText text={provider.wireApi ?? "chat"} /></span><span>·</span><span>{provider.apiKeyCredentialRef ? "系统密钥链" : "未配置密钥"}</span></div>
                </div>
                <div className="provider-row-actions">
                  <button type="button" disabled={busy} onClick={() => setEditorProviderId(provider.id)} className="button-secondary button-small">编辑</button>
                  <button type="button" disabled={busy} onClick={() => activate.mutate(provider.id)} className="button-secondary button-small">{provider.enabled ? "设为唯一当前" : "启用"}</button>
                </div>
              </div>
            ))}
          </div>
        ) : <p className="muted-copy">{shell ? "还没有配置 Provider。" : "在桌面应用中配置 AI Provider。"}</p>}
        {providers.isError ? <div className="settings-error-row" role="alert"><span>{providers.error.message}</span><button type="button" className="text-button" onClick={() => void providers.refetch()}>重试</button></div> : null}
        {activate.isError ? <div className="settings-error-row" role="alert"><span>启用失败：{activate.error.message}</span>{activate.variables ? <button type="button" className="text-button" onClick={() => activate.mutate(activate.variables!)}>重试</button> : null}</div> : null}
      </section>

      {/* Each provider owns its draft. Query refreshes never overwrite typing. */}
      {!providers.isLoading && !providers.isError ? <ProviderEditor key={editorProvider?.id ?? "new"} provider={editorProvider} existingProviderIds={new Set(providers.data?.map((item) => item.id))} onSaved={onSaved} /> : null}
    </div>
  );
}

/** Shared with the first-use guide, which hosts its own draft and provider list. */
export function ProviderEditor({ provider, existingProviderIds, onSaved }: { provider?: AiProviderConfig; existingProviderIds: Set<string>; onSaved: (response: { provider: AiProviderConfig }) => void }) {
  const shell = isDesktopShell();
  const busy = useIsMutating({ mutationKey: ["provider-write"] }) > 0;
  const [providerId, setProviderId] = useState(provider?.id ?? "");
  const [label, setLabel] = useState(provider?.label ?? "");
  const [baseUrl, setBaseUrl] = useState(provider?.baseUrl ?? "");
  const [model, setModel] = useState(provider?.model ?? "");
  const [apiKey, setApiKey] = useState("");
  const [wireApi, setWireApi] = useState<"chat" | "responses">(provider?.wireApi ?? "chat");
  const [saved, setSaved] = useState(false);
  const modelsId = useId();
  const catalog = useQuery({
    queryKey: ["providers", "models", provider?.id], enabled: shell && Boolean(provider),
    queryFn: async () => (await callDesktop(IPC_COMMANDS.providerModels, { providerId: provider!.id })).models, retry: 0,
  });
  const models = catalog.data ?? provider?.models ?? [];
  const save = useMutation({
    mutationKey: ["provider-write"],
    mutationFn: () => {
      const nextProviderId = providerId.trim();
      if (!provider && !nextProviderId) throw new Error("请填写 Provider ID。");
      if (!provider && existingProviderIds.has(nextProviderId)) throw new Error("Provider ID 已存在，请换一个唯一 ID。");
      // 校验交给共享的那条规则，而不是在这里再手写一遍。
      //
      // 原来这里是 `new URL(baseUrl.trim())` 加一句协议判断，有两个洞：
      //   1. URL 本身不合法时 `new URL` 会先抛出一个英文的 TypeError，
      //      下面那句中文提示根本没有机会执行；
      //   2. 它不检查内嵌凭据，所以 `https://user:pass@host` 在界面上是合法的，
      //      提交后却被 IPC 层的 schema 拒掉 —— 一个说不清来源的错误。
      // `HttpBaseUrlSchema` 两条都管，而且和 Rust 侧、和 IPC 层用的是同一份定义。
      if (!HttpBaseUrlSchema.safeParse(baseUrl).success) {
        throw new Error("Base URL 必须是 http(s) 地址，且不能内嵌账号密码。");
      }
      return callDesktop(IPC_COMMANDS.providerSaveOpenai, {
        providerId: nextProviderId || undefined, label: label.trim() || undefined, baseUrl: baseUrl.trim(),
        model: model.trim(), apiKey: apiKey.trim() || undefined, wireApi, models: models.length ? models : undefined,
      });
    },
    onSuccess: (response) => {
      setProviderId(response.provider.id);
      setLabel(response.provider.label);
      setBaseUrl(response.provider.baseUrl);
      setModel(response.provider.model);
      setApiKey("");
      setSaved(true);
      onSaved(response);
    },
  });
  return (
    <section className="settings-card">
      <div className="settings-card-header"><div><p className="eyebrow">自定义 Provider</p><h2>{provider ? "编辑 Provider" : "添加 Provider"}</h2><p>按 OpenCode 的 custom provider 方式填写唯一 ID、名称、Base URL、API Key 和模型。API Key 仅保存在系统密钥链。</p></div>{provider ? <span className="settings-editing">正在编辑 {provider.label}</span> : null}</div>
      <form onSubmit={(event) => { event.preventDefault(); if (shell && !busy) save.mutate(); }} onChange={() => { setSaved(false); save.reset(); }}>
        <fieldset className="settings-form-fields" disabled={busy}>
          <div className="settings-form-grid settings-form-grid-provider">
            <Field label="Provider ID" className="field-wide"><input className="form-input" value={providerId} onChange={(event) => setProviderId(event.target.value.toLowerCase())} placeholder="myprovider" required={!provider} disabled={Boolean(provider)} spellCheck={false} pattern="[a-z0-9][a-z0-9-_]*" title="只能使用小写字母、数字、连字符和下划线，且首字符不能是符号" /></Field>
            <Field label="名称" className="field-wide"><input className="form-input" value={label} onChange={(event) => setLabel(event.target.value)} placeholder="My AI Provider" required /></Field>
            <Field label="Base URL" className="field-wide"><input type="url" className="form-input" value={baseUrl} onChange={(event) => setBaseUrl(event.target.value)} placeholder="https://api.example.com/v1" required spellCheck={false} /></Field>
            <Field label="Model"><input className="form-input" list={modelsId} value={model} onChange={(event) => setModel(event.target.value)} placeholder="选择或输入模型 ID" required spellCheck={false} /><datalist id={modelsId}>{models.map((option) => <option key={option.id} value={option.id}>{option.label}</option>)}</datalist></Field>
            <Field label="Wire API"><select className="form-input" value={wireApi} onChange={(event) => setWireApi(event.target.value as "chat" | "responses")}><option value="chat">Chat Completions</option><option value="responses">Responses</option></select></Field>
            <Field label="API Key" className="field-wide"><input className="form-input" type="password" value={apiKey} onChange={(event) => setApiKey(event.target.value)} autoComplete="new-password" placeholder={provider?.apiKeyCredentialRef ? "留空以保留当前密钥" : "粘贴 API Key（本地无鉴权端点可留空）"} /></Field>
          </div>
        </fieldset>
        {catalog.isError ? <p className="form-hint form-hint-warning">实时模型列表暂不可用，可手动输入模型 ID。</p> : null}
        {save.isError ? <div className="settings-error-row" role="alert"><span>{save.error.message}</span><button type="button" className="text-button" onClick={() => save.mutate()}>重试</button></div> : null}
        {saved ? <p className="form-success" role="status">Provider 配置已保存。</p> : null}
        <div className="settings-actions">
          <button type="submit" disabled={!shell || busy || !baseUrl.trim() || !model.trim()} className="button-primary"><Icon name="connect" size="sm" />{save.isPending ? "保存中…" : provider ? "保存并启用" : "添加并启用"}</button>
          {provider ? <button type="button" disabled={!shell || busy || catalog.isFetching} onClick={() => void catalog.refetch()} className="button-secondary">{catalog.isFetching ? "检查中…" : "刷新模型"}</button> : null}
          {models.length ? <span className="settings-storage-note">{models.length} 个可选模型</span> : null}
        </div>
      </form>
    </section>
  );
}
function Field({ label, className, children }: { label: string; className?: string; children: ReactNode }) {
  return <label className={className ? `settings-field ${className}` : "settings-field"}><span className="field-label">{label}</span>{children}</label>;
}

function Row({ label, value, wide = false }: { label: string; value: string; wide?: boolean }) {
  return <div className={wide ? "settings-fact settings-fact-wide" : "settings-fact"}><dt>{label}</dt><dd>{value}</dd></div>;
}

function unknown(shell: boolean): string {
  return shell ? "—" : "不可用";
}

function text(value: string | number | null | undefined, shell: boolean): string {
  return value === null || value === undefined ? unknown(shell) : String(value);
}

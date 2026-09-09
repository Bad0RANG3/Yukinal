import { useIsMutating, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS, type AiProviderConfig, type CcSwitchProviderCandidate } from "@yukinal/shared";
import { useId, useState, type ReactNode } from "react";

import { Icon } from "../../components/Icon.js";
import { KeywordText } from "../../components/KeywordText.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useAgentLogs, useAgentStatus, useCorePing } from "../../lib/runtime.js";
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
          <Icon name="warning" size={16} />
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
            <Icon name={showLogs ? "chevronUp" : "logs"} size={14} />
            {showLogs ? "隐藏 Agent 日志" : "查看 Agent 日志"}
          </button>
        </div>
        {status.isError || core.isError ? <p className="form-error" role="alert">{status.error?.message ?? core.error?.message}</p> : null}
        {showLogs ? <pre id="runtime-logs" className="agent-log-viewer">{logs.isLoading ? "正在读取日志…" : logs.isError ? logs.error.message : logs.data?.lines.length ? logs.data.lines.join("\n") : "（暂无捕获输出）"}</pre> : null}
      </section>

      <AppearanceSettings />
      <ProviderSettings />
    </section>
  );
}

function AppearanceSettings() {
  const preferences = usePreferencesStore();
  const setPreferences = preferences.setPreferences;

  return (
    <section className="settings-card settings-appearance">
      <div className="settings-card-header">
        <div>
          <p className="eyebrow">偏好</p>
          <h2>外观与交互</h2>
          <p>修改会立即应用，并保存在当前设备的本地存储中。</p>
        </div>
        <Icon name="settings" size={18} />
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
            <option value="jetbrains-mono-nl">JetBrains Mono NL</option>
            <option value="jetbrains-mono">JetBrains Mono</option>
            <option value="jetbrains-nerd-mono">JetBrains Mono Nerd Font</option>
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
      <div className="settings-actions">
        <button type="button" className="button-secondary" onClick={preferences.resetPreferences}>
          <Icon name="refresh" size={14} />恢复默认设置
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
  const providers = useQuery({
    queryKey: ["providers"], enabled: shell,
    queryFn: async () => (await callDesktop(IPC_COMMANDS.providerList, {})).providers,
  });
  const ccswitch = useQuery({
    queryKey: ["providers", "ccswitch"], enabled: shell,
    queryFn: async () => (await callDesktop(IPC_COMMANDS.providerImportCcSwitch, {})).providers, retry: 0,
  });
  const codex = useQuery({
    queryKey: ["providers", "codex"], enabled: shell,
    queryFn: async () => (await callDesktop(IPC_COMMANDS.providerImportCodex, {})).providers, retry: 0,
  });
  const selectedProvider = providers.data?.find((provider) => provider.id === selectedProviderId)
    ?? providers.data?.find((provider) => provider.enabled);
  const onSaved = ({ provider }: { provider: AiProviderConfig }) => {
    selectProvider(provider.id, provider.model);
    void queryClient.invalidateQueries({ queryKey: ["providers"] });
  };
  const activate = useMutation({
    mutationKey: ["provider-write"],
    mutationFn: (providerId: string) => callDesktop(IPC_COMMANDS.providerActivate, { providerId }), onSuccess: onSaved,
  });
  const importCcSwitch = useMutation({
    mutationKey: ["provider-write"],
    mutationFn: (providerId: string) => callDesktop(IPC_COMMANDS.providerImportCcSwitchApply, { ccSwitchProviderId: providerId }), onSuccess: onSaved,
  });
  const importCodex = useMutation({
    mutationKey: ["provider-write"],
    mutationFn: (providerId: string) => callDesktop(IPC_COMMANDS.providerImportCodexApply, { codexProviderId: providerId }), onSuccess: onSaved,
  });
  const autoImport = useMutation({
    mutationKey: ["provider-write"],
    mutationFn: () => callDesktop(IPC_COMMANDS.providerImportAuto, {}),
    onSuccess: (response) => {
      const selected = response.providers.find((provider) => provider.enabled) ?? response.providers[0];
      if (selected) selectProvider(selected.id, selected.model);
      void queryClient.invalidateQueries({ queryKey: ["providers"] });
    },
  });

  return (
    <div className="settings-stack">
      <section className="settings-card">
        <div className="settings-card-header">
          <div><p className="eyebrow">AI</p><h2>Provider</h2><p>新建 Agent 运行会使用当前启用的 Provider。</p></div>
          <span className="settings-count">{providers.data?.length ?? 0} 个已配置</span>
        </div>
        {providers.isLoading ? <p className="muted-copy" role="status">正在加载 Provider…</p> : providers.isError ? <p className="form-error" role="alert">{providers.error.message}</p> : providers.data?.length ? (
          <div className="provider-list">
            {providers.data.map((provider) => (
              <div key={provider.id} className={`provider-row ${provider.enabled ? "provider-row-active" : ""}`}>
                <div className="provider-copy">
                  <div className="provider-name"><span className={`provider-status-dot ${provider.enabled ? "provider-status-on" : "provider-status-off"}`} /><span>{provider.label}</span>{provider.enabled ? <span className="status-badge status-badge-success">当前使用</span> : null}</div>
                  <div className="provider-meta"><span><KeywordText text={provider.model} /></span><span>·</span><span><KeywordText text={provider.wireApi ?? "chat"} /></span><span>·</span><span>{provider.apiKeyCredentialRef ? "系统密钥链" : "未配置密钥"}</span></div>
                </div>
                <button type="button" disabled={busy} onClick={() => activate.mutate(provider.id)} className="button-secondary button-small">{provider.enabled ? "设为唯一当前" : "启用"}</button>
              </div>
            ))}
          </div>
        ) : <p className="muted-copy">{shell ? "还没有配置 Provider。" : "在桌面应用中配置 AI Provider。"}</p>}
        {activate.isError ? <p className="form-error" role="alert">启用失败：{activate.error.message}</p> : null}
      </section>

      {/* Each provider owns its draft. Query refreshes never overwrite typing. */}
      {!providers.isLoading && !providers.isError ? <ProviderEditor key={selectedProvider?.id ?? "new"} provider={selectedProvider} onSaved={onSaved} /> : null}

      <section className="settings-card">
        <div className="settings-card-header"><div><p className="eyebrow">导入</p><h2>本地配置来源</h2><p>启动时会自动同步 OpenCode、Codex 和 CC Switch 配置；也可以在这里手动重新导入。</p></div></div>
        {!shell ? <p className="muted-copy">配置导入需要在桌面应用中使用。</p> : <div className="source-list-stack">
          <SourceList title="CC Switch" state={ccswitch} busy={busy} onImport={(id) => importCcSwitch.mutate(id)} />
          <SourceList title="Codex 配置" state={codex} busy={busy} onImport={(id) => importCodex.mutate(id)} />
        </div>}
        {shell ? <div className="settings-actions">
          <button type="button" className="button-secondary" disabled={busy} onClick={() => autoImport.mutate()}>
            <Icon name="refresh" size={14} />{autoImport.isPending ? "同步中…" : "立即同步本地配置"}
          </button>
          {autoImport.data ? <span className="settings-storage-note">本次同步 {autoImport.data.imported} 个 Provider</span> : null}
        </div> : null}
        {importCcSwitch.isError ? <p className="form-error" role="alert">CC Switch 导入失败：{importCcSwitch.error.message}</p> : null}
        {importCodex.isError ? <p className="form-error" role="alert">Codex 导入失败：{importCodex.error.message}</p> : null}
        {autoImport.isError ? <p className="form-error" role="alert">自动同步失败：{autoImport.error.message}</p> : null}
      </section>
    </div>
  );
}

function ProviderEditor({ provider, onSaved }: { provider?: AiProviderConfig; onSaved: (response: { provider: AiProviderConfig }) => void }) {
  const shell = isDesktopShell();
  const busy = useIsMutating({ mutationKey: ["provider-write"] }) > 0;
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
      const url = new URL(baseUrl.trim());
      if (url.protocol !== "https:" && url.protocol !== "http:") throw new Error("Base URL 必须使用 HTTP 或 HTTPS。");
      return callDesktop(IPC_COMMANDS.providerSaveOpenai, {
        providerId: provider?.id, label: label.trim() || undefined, baseUrl: baseUrl.trim(),
        model: model.trim(), apiKey: apiKey.trim() || undefined, wireApi, models: models.length ? models : undefined,
      });
    },
    onSuccess: (response) => {
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
      <div className="settings-card-header"><div><p className="eyebrow">连接配置</p><h2>Provider 配置</h2><p>修改在保存后生效，API Key 仅保存在系统密钥链。</p></div>{provider ? <span className="settings-editing">正在编辑 {provider.label}</span> : null}</div>
      <form onSubmit={(event) => { event.preventDefault(); if (shell && !busy) save.mutate(); }} onChange={() => { setSaved(false); save.reset(); }}>
        <fieldset className="settings-form-fields" disabled={busy}>
          <div className="settings-form-grid settings-form-grid-provider">
            <Field label="名称" className="field-wide"><input className="form-input" value={label} onChange={(event) => setLabel(event.target.value)} placeholder="公司网关、Ollama…" /></Field>
            <Field label="Base URL" className="field-wide"><input type="url" className="form-input" value={baseUrl} onChange={(event) => setBaseUrl(event.target.value)} placeholder="https://api.example.com/v1" required spellCheck={false} /></Field>
            <Field label="Model"><input className="form-input" list={modelsId} value={model} onChange={(event) => setModel(event.target.value)} placeholder="选择或输入模型 ID" required spellCheck={false} /><datalist id={modelsId}>{models.map((option) => <option key={option.id} value={option.id}>{option.label}</option>)}</datalist></Field>
            <Field label="Wire API"><select className="form-input" value={wireApi} onChange={(event) => setWireApi(event.target.value as "chat" | "responses")}><option value="chat">Chat Completions</option><option value="responses">Responses</option></select></Field>
            <Field label="API Key" className="field-wide"><input className="form-input" type="password" value={apiKey} onChange={(event) => setApiKey(event.target.value)} autoComplete="new-password" placeholder={provider?.apiKeyCredentialRef ? "留空以保留当前密钥" : "本地端点可留空"} /></Field>
          </div>
        </fieldset>
        {catalog.isError ? <p className="form-hint form-hint-warning">实时模型列表暂不可用，可手动输入模型 ID。</p> : null}
        {save.isError ? <p className="form-error" role="alert">{save.error.message}</p> : null}
        {saved ? <p className="form-success" role="status">Provider 配置已保存。</p> : null}
        <div className="settings-actions">
          <button type="submit" disabled={!shell || busy || !baseUrl.trim() || !model.trim()} className="button-primary"><Icon name="connect" size={14} />{save.isPending ? "保存中…" : "保存 Provider"}</button>
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

function SourceList({ title, state, busy, onImport }: { title: string; state: { data?: CcSwitchProviderCandidate[]; isLoading: boolean; isError: boolean }; busy: boolean; onImport: (id: string) => void }) {
  if (state.isLoading) return <p className="muted-copy">{title}：正在扫描…</p>;
  if (state.isError) return <p className="muted-copy">{title}：未找到或无法读取</p>;
  if (!state.data?.length) return <p className="muted-copy">{title}：暂无配置</p>;
  return (
    <div className="source-group">
      <div className="source-title">{title}</div>
      <div className="source-items">
        {state.data.map((candidate) => (
          <div key={candidate.id} className="source-item">
            <span className="source-item-copy">{candidate.name} · {candidate.model}<small>· {candidate.models?.length ?? 0} 个模型 · {candidate.wireApi} · {candidate.hasApiKey ? "有密钥" : "无密钥"}</small></span>
            <button type="button" disabled={busy} onClick={() => onImport(candidate.id)} className="button-secondary button-small">{busy ? "导入中…" : "导入"}</button>
          </div>
        ))}
      </div>
    </div>
  );
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

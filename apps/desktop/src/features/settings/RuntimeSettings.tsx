import { useIsMutating, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  HttpBaseUrlSchema,
  IPC_COMMANDS,
  type AiProviderConfig,
  type AiProviderKind,
  type ProviderModelDiscoveryInput,
  type ProviderModelOption,
} from "@yukinal/shared";
import { useId, useState, type ReactNode } from "react";

import { Icon } from "../../components/Icon.js";
import { McpSettings } from "./McpSettings.js";
import { NetworkSettings } from "./NetworkSettings.js";
import { NEW_PROVIDER_VALUE, ProviderPicker } from "./ProviderPicker.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import {
  PROVIDERS_QUERY_KEY,
  PROVIDER_KIND_DEFAULT_BASE_URL,
  PROVIDER_KIND_LABEL,
  PROVIDER_KIND_ORDER,
  providerDeleteNotice,
  providerHeadersText,
  providerKindUsesApiVersion,
  providerKindUsesWireApi,
  parseProviderHeaders,
  providerSavePayload,
  providerModelsQueryOptions,
  useProviders,
} from "../../lib/providers.js";
import { useAgentLogs, useAgentStatus, useCorePing } from "../../lib/runtime.js";
import { usePresence } from "../../hooks/usePresence.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { PreferenceSettings } from "./PreferenceSettings.js";

const SETTINGS_SECTIONS = [
  { id: "general", label: "偏好", description: "终端和任务批准偏好，修改后自动保存到本机。" },
  { id: "models", label: "模型", description: "管理模型连接。切换查看不会更换当前使用的连接。" },
  { id: "tools", label: "工具", description: "管理 MCP 连接与工具授权。保存配置不会自动启动。" },
  { id: "network", label: "网络", description: "管理出站连接方式，保存后下次连接生效。" },
  { id: "runtime", label: "诊断", description: "查看运行服务的状态、版本和日志，排查连接与执行问题。" },
] as const;

export function RuntimeSettings() {
  const [section, setSection] = useState<(typeof SETTINGS_SECTIONS)[number]["id"]>("general");
  const [visited, setVisited] = useState<ReadonlySet<string>>(() => new Set(["general"]));
  const selectSection = (next: (typeof SETTINGS_SECTIONS)[number]["id"]) => {
    setVisited((current) => current.has(next) ? current : new Set([...current, next]));
    setSection(next);
  };
  const id = useId();
  return <section className="settings-page settings-organized">
    <div className="settings-section-tabs" role="tablist" aria-label="设置分区">
      {SETTINGS_SECTIONS.map((item, index) => <button type="button" role="tab" key={item.id}
        id={`${id}-tab-${item.id}`} aria-controls={`${id}-panel-${item.id}`}
        aria-selected={section === item.id} tabIndex={section === item.id ? 0 : -1}
        onClick={() => selectSection(item.id)} onKeyDown={(event) => {
          const next = event.key === "ArrowRight" ? (index + 1) % SETTINGS_SECTIONS.length
            : event.key === "ArrowLeft" ? (index + SETTINGS_SECTIONS.length - 1) % SETTINGS_SECTIONS.length
            : event.key === "Home" ? 0 : event.key === "End" ? SETTINGS_SECTIONS.length - 1 : null;
          const target = next === null ? null : SETTINGS_SECTIONS[next];
          if (target) { event.preventDefault(); selectSection(target.id); document.getElementById(`${id}-tab-${target.id}`)?.focus(); }
        }}>{item.label}</button>)}
    </div>
    {SETTINGS_SECTIONS.map((item) => <div key={item.id} id={`${id}-panel-${item.id}`}
      role="tabpanel" aria-labelledby={`${id}-tab-${item.id}`} hidden={section !== item.id} tabIndex={0}>
      {/* Keep visited editors mounted so switching tabs preserves unsaved drafts. */}
      {visited.has(item.id) ? item.id === "general" ? <PreferenceSettings /> : item.id === "models" ? <ProviderSettings active={section === item.id} />
        : item.id === "tools" ? <McpSettings active={section === item.id} /> : item.id === "network" ? <NetworkSettings active={section === item.id} /> : <RuntimeStatus active={section === item.id} /> : null}
    </div>)}
  </section>;
}

function RuntimeStatus({ active }: { active: boolean }) {
  const [showLogs, setShowLogs] = useState(false);
  const core = useCorePing({ enabled: active });
  const status = useAgentStatus({ enabled: active });
  const logs = useAgentLogs(active && showLogs);
  const shell = isDesktopShell();
  return (
    <section className="settings-card">
        <div className="settings-card-header">
          <div>
            <h2>运行服务</h2>
          </div>
          <span className={`settings-status ${status.data?.running ? "settings-status-ready" : ""}`}>
            {!shell ? "预览模式" : status.isError ? "状态读取失败" : status.isLoading ? "查询中…" : status.data?.running ? "运行中" : "未启动"}
          </span>
        </div>
        <dl className="settings-facts">
          <Row label="核心版本" value={core.data ? `${core.data.version} (${core.data.os})` : unknown(shell)} />
          <Row label="运行服务" value={status.data ? (status.data.running ? "运行中" : "已停止") : unknown(shell)} />
          <Row label="进程" value={text(status.data?.pid, shell)} />
          <Row label="协议" value={text(status.data?.protocolVersion, shell)} />
          <Row label="服务版本" value={text(status.data?.agentVersion, shell)} />
          <Row label="工具" value={text(status.data?.toolCount, shell)} />
          <Row label="服务入口" value={text(status.data?.entry, shell)} wide />
          <Row label="上次退出" value={status.data?.lastExit ? `${status.data.lastExit.code ?? status.data.lastExit.signal ?? "未知"} @ ${status.data.lastExit.at}` : "无"} wide />
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
        </dl>
        <div className="settings-card-actions">
          <button type="button" className="button-secondary" disabled={!shell} aria-expanded={showLogs} aria-controls="runtime-logs" onClick={() => setShowLogs((current) => !current)}>
            <Icon name={showLogs ? "chevronUp" : "logs"} size="sm" />
            {showLogs ? "隐藏日志" : "查看日志"}
          </button>
        </div>
        {status.isError || core.isError ? <div className="settings-error-row" role="alert"><span>{status.error?.message ?? core.error?.message}</span><button type="button" className="text-button" onClick={() => { void core.refetch(); void status.refetch(); }}>重试</button></div> : null}
        <RuntimeLogs open={showLogs} logs={logs} />
      </section>
  );
}

/**
 * 「查看日志」展开的那块输出。
 *
 * 从 RuntimeSettings 里拆出来有两个理由：一是它有三种互斥的形态（加载中 /
 * 读取失败 / 有内容），挤在一行里既读不了也改不动；二是它需要一段收起动画，
 * 而 presence 要求把 `is-closing` 和 `onAnimationEnd` 交给**当前真正渲染出来的
 * 那个元素** —— 内联写在三元表达式里做不到这件事。
 */
export function RuntimeLogs({ open, logs }: { open: boolean; logs: ReturnType<typeof useAgentLogs> }) {
  const presence = usePresence(open, { exitAnimation: "expand-exit" });
  if (!presence.mounted) return null;
  const closing = presence.closing ? " is-closing" : "";
  if (logs.isLoading) return <pre id="runtime-logs" className={`agent-log-viewer${closing}`} onAnimationEnd={presence.onAnimationEnd}>正在读取日志…</pre>;
  if (logs.isError) {
    return (
      <div id="runtime-logs" className={`settings-error-row${closing}`} role="alert" onAnimationEnd={presence.onAnimationEnd}>
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

function ProviderSettings({ active }: { active: boolean }) {
  const queryClient = useQueryClient();
  const shell = isDesktopShell();
  const busy = useIsMutating({ mutationKey: ["provider-write"] }) > 0;
  const selectedProviderId = useWorkspaceStore((state) => state.selectedProviderId);
  const selectProvider = useWorkspaceStore((state) => state.selectProvider);
  // `undefined` follows the active provider, `null` starts a new configuration,
  // and an id keeps a specific saved provider open for editing.
  const [editorProviderId, setEditorProviderId] = useState<string | null | undefined>(undefined);
  const providers = useProviders({ enabled: active });
  const selectedProvider = providers.data?.find((provider) => provider.id === selectedProviderId)
    ?? providers.data?.find((provider) => provider.enabled);
  const editorProvider = editorProviderId === undefined
    ? selectedProvider
    : editorProviderId === null
      ? undefined
      : providers.data?.find((provider) => provider.id === editorProviderId);
  // 选择框里正在看的那一个。没选过时跟着当前启用的 Provider：首屏不该先要求用户选一次
  // 才能看见任何东西。
  const viewingProviderId = editorProviderId === undefined
    ? selectedProvider?.id ?? NEW_PROVIDER_VALUE
    : editorProviderId ?? NEW_PROVIDER_VALUE;
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
  /**
   * 删除：确认块由 `confirmingId` 控制（确认必须在**静态渲染**里也能被测到，所以它不由
   * 这个控件自己藏着）。
   *
   * 删掉之后不在这里给 `selectedProviderId` 补一个值：那条「列表刷新后当前选择已失效」的
   * 回落规则只住在一处（`useAgentModels.ts`），在这里再写一份就是第二份说法。这里只把
   * 「在看哪一个」交回给默认回落（当前启用的那个）。
   */
  const [confirmingId, setConfirmingId] = useState<string | null>(null);
  const [removed, setRemoved] = useState<string | null>(null);
  const remove = useMutation({
    mutationKey: ["provider-write"],
    // 变量收整个 Provider（而不是一个 id）：结果提示要说的是「哪个 Provider 被删了」，
    // 而名称只有这一份。
    mutationFn: (provider: AiProviderConfig) => callDesktop(IPC_COMMANDS.providerDelete, { providerId: provider.id }),
    onSuccess: ({ credentialReclaimed }, provider) => {
      setConfirmingId(null);
      setRemoved(providerDeleteNotice(provider, credentialReclaimed));
      setEditorProviderId(undefined);
      void queryClient.invalidateQueries({ queryKey: PROVIDERS_QUERY_KEY });
    },
  });
  return (
    <div className="settings-stack">
      <section className="settings-card">
        <div className="settings-card-header">
          <div><h2>模型连接</h2></div>
          <div className="settings-card-header-actions">{providers.data ? <span className="settings-count">{providers.data.length} 个已配置</span> : null}<button type="button" disabled={busy || providers.isLoading || providers.isError} title={providers.isError ? "读取连接失败，请重试后添加" : undefined} onClick={() => setEditorProviderId(null)} className="button-secondary button-small">添加连接</button></div>
        </div>
        {providers.isLoading ? <p className="muted-copy" role="status">正在加载连接…</p> : providers.isError ? null : providers.data?.length ? (
          <ProviderPicker
            providers={providers.data}
            viewingId={viewingProviderId}
            busy={busy}
            confirmingId={confirmingId}
            // 「新建」在选择框里是一个取值，而不是一个并列的按钮：否则点开「添加」之后
            // 选择框还指着上一个 Provider，两处说法就对不上了。
            onView={(providerId) => {
              setRemoved(null);
              setConfirmingId(null);
              setEditorProviderId(providerId === NEW_PROVIDER_VALUE ? null : providerId);
            }}
            onActivate={(providerId) => activate.mutate(providerId)}
            onRequestDelete={(providerId) => {
              setRemoved(null);
              setConfirmingId(providerId);
            }}
            onCancelDelete={() => setConfirmingId(null)}
            onConfirmDelete={() => {
              const target = providers.data?.find((provider) => provider.id === viewingProviderId);
              if (target && !remove.isPending) remove.mutate(target);
            }}
          />
        ) : <p className="muted-copy">{shell ? "暂无模型连接" : "桌面功能不可用"}</p>}
        {providers.isError ? <div className="settings-error-row" role="alert"><span>{providers.error.message}</span><button type="button" className="text-button" onClick={() => void providers.refetch()}>重试</button></div> : null}
        {activate.isError ? <div className="settings-error-row" role="alert"><span>启用失败：{activate.error.message}</span>{activate.variables ? <button type="button" className="text-button" onClick={() => activate.mutate(activate.variables!)}>重试</button> : null}</div> : null}
        {remove.isError ? <div className="settings-error-row" role="alert"><span>删除失败：{remove.error.message}</span>{remove.variables ? <button type="button" className="text-button" onClick={() => remove.mutate(remove.variables!)}>重试</button> : null}</div> : null}
        {removed ? <p className="settings-storage-note provider-picker-notice" role="status">{removed}</p> : null}
      </section>

      {/* Each provider owns its draft. Query refreshes never overwrite typing. */}
      {!providers.isLoading && !providers.isError ? <ProviderEditor key={editorProvider?.id ?? "new"} active={active} provider={editorProvider} existingProviderIds={new Set(providers.data?.map((item) => item.id))} onSaved={onSaved} /> : null}
    </div>
  );
}

/** Shared with the first-use guide, which hosts its own draft and provider list. */
export function ProviderEditor({ provider, existingProviderIds, onSaved, active = true }: { provider?: AiProviderConfig; existingProviderIds: Set<string>; onSaved: (response: { provider: AiProviderConfig }) => void; active?: boolean }) {
  const shell = isDesktopShell();
  const busy = useIsMutating({ mutationKey: ["provider-write"] }) > 0;
  const [kind, setKind] = useState<AiProviderKind>(provider?.kind ?? "openai-compatible");
  const [label, setLabel] = useState(provider?.label ?? "");
  const [baseUrl, setBaseUrl] = useState(provider?.baseUrl ?? "");
  const [model, setModel] = useState(provider?.model ?? "");
  const [apiKey, setApiKey] = useState("");
  const [wireApi, setWireApi] = useState<"chat" | "responses">(provider?.wireApi ?? "chat");
  const [apiVersion, setApiVersion] = useState(provider?.apiVersion ?? "");
  const [customHeadersText, setCustomHeadersText] = useState(providerHeadersText(provider?.customHeaders));
  const [discoveredModels, setDiscoveredModels] = useState<ProviderModelOption[]>([]);
  const [catalogSource, setCatalogSource] = useState<"saved" | "draft">(provider ? "saved" : "draft");
  const [saved, setSaved] = useState(false);
  const catalog = useQuery(providerModelsQueryOptions(provider?.id, { enabled: active }));
  const models = catalogSource === "saved"
    ? catalog.data ?? provider?.models ?? []
    : discoveredModels;
  const discoverModels = useMutation({
    mutationFn: async () => {
      if (!HttpBaseUrlSchema.safeParse(baseUrl).success) {
        throw new Error("Base URL 必须是有效的安全 http(s) 地址。");
      }
      const input: ProviderModelDiscoveryInput = {
        providerId: provider?.id,
        kind,
        baseUrl: baseUrl.trim(),
        apiKey: apiKey.trim() || undefined,
        customHeaders: parseProviderHeaders(customHeadersText),
        apiVersion: providerKindUsesApiVersion(kind) ? apiVersion.trim() || undefined : undefined,
      };
      const response = await callDesktop(IPC_COMMANDS.providerModels, { input });
      if (response.models.length === 0) {
        throw new Error("服务商没有返回可用模型，请检查协议、地址和密钥。");
      }
      return response.models;
    },
    onSuccess: (nextModels) => {
      setDiscoveredModels(nextModels);
      setCatalogSource("draft");
      setModel((current) =>
        nextModels.some((option) => option.id === current) ? current : "",
      );
    },
  });
  const clearDiscoveredModels = () => {
    setDiscoveredModels([]);
    setCatalogSource("draft");
    discoverModels.reset();
  };
  /**
   * 切换协议。
   *
   * kind 决定「由谁翻译」，所以它一变，base URL 的含义也变了；但**绝不覆盖用户手写的地址**。
   * 只有当字段是空的、或还停在某个协议自己的默认端点上时才接管 —— 否则切一次协议就会把
   * 用户填的网关地址悄悄换掉，而那个地址是唯一能解释「请求到底发去哪儿」的东西。
   */
  const changeKind = (next: AiProviderKind) => {
    const current = baseUrl.trim();
    const untouched =
      current === "" ||
      Object.values(PROVIDER_KIND_DEFAULT_BASE_URL).includes(current);
    setKind(next);
    if (untouched) setBaseUrl(PROVIDER_KIND_DEFAULT_BASE_URL[next] ?? "");
    clearDiscoveredModels();
  };
  const save = useMutation({
    mutationKey: ["provider-write"],
    mutationFn: () => {
      if (!label.trim()) throw new Error("请填写连接名称。");
      const nextProviderId = provider?.id ?? makeProviderId(label, existingProviderIds);
      // 校验交给共享的那条规则，而不是在这里再手写一遍。
      //
      // 原来这里是 `new URL(baseUrl.trim())` 加一句协议判断，有两个洞：
      //   1. URL 本身不合法时 `new URL` 会先抛出一个英文的 TypeError，
      //      下面那句中文提示根本没有机会执行；
      //   2. 它不检查内嵌凭据，所以 `https://user:pass@host` 在界面上是合法的，
      //      提交后却被 IPC 层的 schema 拒掉 —— 一个说不清来源的错误。
      // `HttpBaseUrlSchema` 两条都管，而且和宿主侧、和 IPC 层用的是同一份定义。
      if (!HttpBaseUrlSchema.safeParse(baseUrl).success) {
        throw new Error("Base URL 必须是 http(s) 地址，且不能内嵌账号密码。");
      }
      const customHeaders = parseProviderHeaders(customHeadersText);
      // payload 形状（kind 必带、wireApi 只在 openai-compatible 出现）由那一个纯函数决定，
      // 而不是在这里写第二遍 —— 它同时被用例钉住。
      return callDesktop(IPC_COMMANDS.providerSave, providerSavePayload({
        providerId: nextProviderId, kind, label, baseUrl, model, apiKey, wireApi, apiVersion,
        customHeaders, models,
      }));
    },
    onSuccess: (response) => {
      setKind(response.provider.kind);
      setLabel(response.provider.label);
      setBaseUrl(response.provider.baseUrl);
      setModel(response.provider.model);
      setApiKey("");
      setApiVersion(response.provider.apiVersion ?? "");
      setCustomHeadersText(providerHeadersText(response.provider.customHeaders));
      setDiscoveredModels(response.provider.models ?? []);
      setCatalogSource("draft");
      setSaved(true);
      onSaved(response);
    },
  });
  return (
    <section className="settings-card">
      <div className="settings-card-header"><div><h2>{provider ? "编辑连接" : "添加连接"}</h2></div>{provider ? <span className="settings-editing">正在编辑 {provider.label}</span> : null}</div>
      <form onSubmit={(event) => { event.preventDefault(); if (shell && !busy) save.mutate(); }} onChange={() => { setSaved(false); save.reset(); }}>
        <fieldset className="settings-form-fields" disabled={busy || discoverModels.isPending}>
          <div className="settings-form-grid settings-form-grid-provider">
            <Field label="名称" className="field-wide"><input className="form-input" value={label} onChange={(event) => setLabel(event.target.value)} placeholder="连接名称" required /></Field>
            <Field label="协议" className="field-wide"><select className="form-input" value={kind} onChange={(event) => changeKind(event.target.value as AiProviderKind)}>{PROVIDER_KIND_ORDER.map((option) => <option key={option} value={option}>{PROVIDER_KIND_LABEL[option]}</option>)}</select></Field>
            <Field label="Base URL" className="field-wide"><input type="url" className="form-input" value={baseUrl} onChange={(event) => { setBaseUrl(event.target.value); clearDiscoveredModels(); }} placeholder={PROVIDER_KIND_DEFAULT_BASE_URL[kind] ?? "https://api.example.com/v1"} required spellCheck={false} /></Field>
            <Field label="API Key" className="field-wide"><input className="form-input" type="password" value={apiKey} onChange={(event) => { setApiKey(event.target.value); clearDiscoveredModels(); }} autoComplete="new-password" placeholder={provider?.apiKeyCredentialRef ? "留空以保留当前密钥" : "粘贴 API Key（本地无鉴权端点可留空）"} /></Field>
            <Field label="模型"><div className="provider-model-field">
              <div className="provider-model-control">{models.length ? <select className="form-input" value={model} onChange={(event) => setModel(event.target.value)} required>
                <option value="" disabled>选择服务商提供的模型</option>
                {model && !models.some((option) => option.id === model) ? <option value={model}>{model}（当前配置）</option> : null}
                {models.map((option) => <option key={option.id} value={option.id}>{option.label}</option>)}
              </select> : <input className="form-input" value={model} onChange={(event) => setModel(event.target.value)} placeholder="获取列表，或手动输入模型 ID" required spellCheck={false} />}
                <button type="button" disabled={!shell || busy || !baseUrl.trim() || discoverModels.isPending} onClick={() => discoverModels.mutate()} className="button-secondary">{discoverModels.isPending ? "正在获取…" : "获取模型"}</button>
              </div>
              {!baseUrl.trim() ? <span className="form-hint">先填写实际 Base URL；需要鉴权的服务商还需填 API Key。</span> : null}
            </div></Field>
          </div>
          <details className="settings-advanced">
            <summary>高级连接选项</summary>
            <div className="settings-form-grid settings-form-grid-provider">
              {providerKindUsesWireApi(kind) ? <Field label="对话接口格式"><select className="form-input" value={wireApi} onChange={(event) => setWireApi(event.target.value as "chat" | "responses")}><option value="chat">Chat Completions</option><option value="responses">Responses</option></select></Field> : null}
              {providerKindUsesApiVersion(kind) ? <Field label="Anthropic Version"><input className="form-input" value={apiVersion} onChange={(event) => { setApiVersion(event.target.value); clearDiscoveredModels(); }} placeholder="2023-06-01（留空使用适配器默认值）" spellCheck={false} /></Field> : null}
              <Field label="自定义请求头" className="field-wide"><textarea className="form-input form-textarea" rows={3} value={customHeadersText} onChange={(event) => { setCustomHeadersText(event.target.value); clearDiscoveredModels(); }} placeholder={"每行一个「名称: 值」，例如\nHTTP-Referer: https://desktop.example"} spellCheck={false} /></Field>
            </div>
          </details>
        </fieldset>
        {catalogSource === "saved" && catalog.isError ? <p className="form-hint form-hint-warning">已保存的模型列表暂不可用；获取新列表或手动输入模型 ID。</p> : null}
        {discoverModels.isError ? <div className="settings-error-row" role="alert"><span>{discoverModels.error.message} 仍可手动输入模型 ID。</span><button type="button" className="text-button" onClick={() => discoverModels.mutate()}>重试</button></div> : null}
        {save.isError ? <div className="settings-error-row" role="alert"><span>{save.error.message}</span><button type="button" className="text-button" onClick={() => save.mutate()}>重试</button></div> : null}
        {saved ? <p className="form-success" role="status">已保存</p> : null}
        <div className="settings-actions">
          <button type="submit" disabled={!shell || busy || discoverModels.isPending || !baseUrl.trim() || !model.trim()} className="button-primary"><Icon name="connect" size="sm" />{save.isPending ? "保存中…" : provider ? "保存并启用" : "添加并启用"}</button>
          {models.length ? <span className="settings-storage-note">{models.length} 个可选模型</span> : null}
        </div>
      </form>
    </section>
  );
}
function Field({ label, className, children }: { label: string; className?: string; children: ReactNode }) {
  return <label className={className ? `settings-field ${className}` : "settings-field"}><span className="field-label">{label}</span>{children}</label>;
}

function makeProviderId(label: string, existingIds: Set<string>): string {
  const base = label.normalize("NFKD").replace(/[\u0300-\u036f]/g, "").toLowerCase()
    .replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 220) || "provider";
  let candidate = base;
  let suffix = 2;
  while (existingIds.has(candidate)) {
    const ending = `-${suffix++}`;
    candidate = `${base.slice(0, 256 - ending.length).replace(/[-_]+$/g, "")}${ending}`;
  }
  return candidate;
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

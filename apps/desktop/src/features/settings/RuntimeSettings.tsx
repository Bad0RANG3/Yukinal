import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS, type CcSwitchProviderCandidate } from "@yukinal/shared";
import { useEffect, useState, type ReactNode } from "react";

import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useAgentLogs, useAgentStatus, useCorePing } from "../../lib/runtime.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";

export function RuntimeSettings() {
  const [showLogs, setShowLogs] = useState(false);
  const core = useCorePing();
  const status = useAgentStatus();
  const logs = useAgentLogs(showLogs);
  const shell = isDesktopShell();

  return (
    <section className="max-w-3xl space-y-4">
      <h1 className="text-lg font-semibold">Runtime and AI</h1>
      {!shell ? <p className="rounded border border-amber-500/40 bg-amber-500/5 p-2 text-sm text-amber-200">Browser preview: native runtime calls are unavailable. Start the Tauri shell to connect an Agent.</p> : null}
      <dl className="grid grid-cols-2 gap-x-4 gap-y-2 text-sm">
        <Row label="Core" value={core.data ? `${core.data.version} (${core.data.os})` : unknown(shell)} />
        <Row label="Agent" value={status.data ? (status.data.running ? "Running" : "Stopped") : unknown(shell)} />
        <Row label="Agent PID" value={text(status.data?.pid, shell)} />
        <Row label="Protocol" value={text(status.data?.protocolVersion, shell)} />
        <Row label="Agent version" value={text(status.data?.agentVersion, shell)} />
        <Row label="Tools" value={text(status.data?.toolCount, shell)} />
        <Row label="Sidecar entry" value={text(status.data?.entry, shell)} wide />
        <Row label="Last exit" value={status.data?.lastExit ? `${status.data.lastExit.code ?? status.data.lastExit.signal ?? "unknown"} @ ${status.data.lastExit.at}` : "None"} wide />
      </dl>
      <div>
        <button type="button" onClick={() => setShowLogs((current) => !current)} className="rounded border border-zinc-700 px-2 py-1 text-xs text-zinc-300">{showLogs ? "Hide Agent logs" : "View Agent logs"}</button>
        {showLogs ? <pre className="mt-2 max-h-64 overflow-auto rounded border border-zinc-800 bg-black/40 p-2 text-[11px] text-zinc-400">{logs.data?.lines.length ? logs.data.lines.join("\n") : "(no captured output)"}</pre> : null}
      </div>
      <ProviderSettings />
    </section>
  );
}

function ProviderSettings() {
  const queryClient = useQueryClient();
  const [label, setLabel] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [model, setModel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [wireApi, setWireApi] = useState<"chat" | "responses">("chat");
  const [saved, setSaved] = useState(false);
  const selectedProviderId = useWorkspaceStore((state) => state.selectedProviderId);
  const selectedModel = useWorkspaceStore((state) => state.selectedModel);
  const selectProvider = useWorkspaceStore((state) => state.selectProvider);
  const selectModel = useWorkspaceStore((state) => state.selectModel);

  const providers = useQuery({
    queryKey: ["providers"],
    enabled: isDesktopShell(),
    queryFn: async () => (await callDesktop(IPC_COMMANDS.providerList, {})).providers,
  });
  const ccswitch = useQuery({
    queryKey: ["providers", "ccswitch"],
    enabled: isDesktopShell(),
    queryFn: async () => (await callDesktop(IPC_COMMANDS.providerImportCcSwitch, {})).providers,
    retry: 0,
  });
  const codex = useQuery({
    queryKey: ["providers", "codex"],
    enabled: isDesktopShell(),
    queryFn: async () => (await callDesktop(IPC_COMMANDS.providerImportCodex, {})).providers,
    retry: 0,
  });
  const selectedProvider = providers.data?.find((provider) => provider.id === selectedProviderId);
  const modelCatalog = useQuery({
    queryKey: ["providers", "models", selectedProviderId],
    enabled: isDesktopShell() && Boolean(selectedProviderId),
    queryFn: async () => (await callDesktop(IPC_COMMANDS.providerModels, { providerId: selectedProviderId as string })).models,
    retry: 0,
  });
  const catalogModels = modelCatalog.data ?? selectedProvider?.models ?? [];
  const models = selectedProvider && !catalogModels.some((option) => option.id === selectedProvider.model)
    ? [{ id: selectedProvider.model, label: selectedProvider.model, supportsToolCalling: true, supportsStreaming: true }, ...catalogModels]
    : catalogModels;

  useEffect(() => {
    if (!providers.data?.length) return;
    const provider = providers.data.find((item) => item.id === selectedProviderId) ?? providers.data.find((item) => item.enabled) ?? providers.data[0];
    if (!provider) return;
    if (provider.id !== selectedProviderId) selectProvider(provider.id, provider.model);
    setLabel(provider.label);
    setBaseUrl(provider.baseUrl);
    setModel(selectedModel ?? provider.model);
    setWireApi(provider.wireApi ?? "chat");
  }, [providers.data, selectedProviderId, selectedModel, selectProvider]);

  const invalidateProviders = () => { void queryClient.invalidateQueries({ queryKey: ["providers"] }); };
  const activate = useMutation({
    mutationFn: (providerId: string) => callDesktop(IPC_COMMANDS.providerActivate, { providerId }),
    onSuccess: ({ provider }) => { selectProvider(provider.id, provider.model); invalidateProviders(); },
  });
  const importCcSwitch = useMutation({
    mutationFn: (providerId: string) => callDesktop(IPC_COMMANDS.providerImportCcSwitchApply, { ccSwitchProviderId: providerId }),
    onSuccess: ({ provider }) => { selectProvider(provider.id, provider.model); invalidateProviders(); },
  });
  const importCodex = useMutation({
    mutationFn: (providerId: string) => callDesktop(IPC_COMMANDS.providerImportCodexApply, { codexProviderId: providerId }),
    onSuccess: ({ provider }) => { selectProvider(provider.id, provider.model); invalidateProviders(); },
  });
  const save = useMutation({
    mutationFn: () => callDesktop(IPC_COMMANDS.providerSaveOpenai, {
      providerId: selectedProviderId ?? undefined,
      label: label.trim() || undefined,
      baseUrl: baseUrl.trim(),
      model: model.trim(),
      apiKey: apiKey.trim() || undefined,
      wireApi,
      models: models.length ? models : undefined,
    }),
    onSuccess: ({ provider }) => { selectProvider(provider.id, provider.model); invalidateProviders(); setSaved(true); setApiKey(""); },
  });

  const input = "w-full rounded-md border border-zinc-800 bg-zinc-900 p-2 text-sm outline-none focus:border-zinc-600 placeholder:text-zinc-600";
  return (
    <div className="space-y-4">
      <div className="rounded-lg border border-zinc-800 p-4">
        <div className="mb-3 flex items-center justify-between"><div><h2 className="text-sm font-medium">AI Providers</h2><p className="mt-1 text-xs text-zinc-500">The active provider is used for new Agent runs.</p></div><span className="text-xs text-zinc-500">{providers.data?.length ?? 0} configured</span></div>
        {providers.data?.length ? <div className="space-y-2">{providers.data.map((provider) => <div key={provider.id} className={`flex items-center gap-3 rounded border p-3 ${provider.id === selectedProviderId ? "border-emerald-500/50 bg-emerald-500/5" : "border-zinc-800"}`}>
          <div className="min-w-0 flex-1"><div className="flex items-center gap-2 text-sm text-zinc-200"><span className={`h-2 w-2 rounded-full ${provider.enabled ? "bg-emerald-400" : "bg-zinc-600"}`} /><span className="truncate">{provider.label}</span>{provider.id === selectedProviderId ? <span className="text-[10px] uppercase tracking-wide text-emerald-300">active</span> : null}</div><div className="mt-1 truncate text-xs text-zinc-500">{provider.model} · {provider.wireApi ?? "chat"} · {provider.apiKeyCredentialRef ? "keychain" : "no key"}</div></div>
          <button type="button" disabled={activate.isPending || provider.id === selectedProviderId} onClick={() => activate.mutate(provider.id)} className="rounded border border-zinc-700 px-2 py-1 text-xs text-zinc-300 disabled:opacity-40">{provider.id === selectedProviderId ? "Using" : "Use"}</button>
        </div>)}</div> : <p className="text-xs text-zinc-500">No provider configured yet.</p>}
      </div>

      <div className="rounded-lg border border-zinc-800 p-4">
        <div className="mb-3 flex items-center justify-between"><div><h2 className="text-sm font-medium">Provider configuration</h2><p className="mt-1 text-xs text-zinc-500">API keys stay in the OS keychain. Only metadata is persisted.</p></div>{selectedProvider ? <span className="text-xs text-emerald-300">Editing {selectedProvider.label}</span> : null}</div>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Name" className="col-span-2"><input className={input} value={label} onChange={(event) => { setLabel(event.target.value); setSaved(false); }} placeholder="OpenRouter, Ollama, company gateway" /></Field>
          <Field label="Base URL" className="col-span-2"><input className={input} value={baseUrl} onChange={(event) => { setBaseUrl(event.target.value); setSaved(false); }} placeholder="https://api.openai.com/v1" /></Field>
          <Field label="Model">{models.length ? <select className={input} value={model} onChange={(event) => { setModel(event.target.value); selectModel(event.target.value); setSaved(false); }}>{models.map((option) => <option key={option.id} value={option.id}>{option.label} · {option.id}</option>)}</select> : <input className={input} value={model} onChange={(event) => { setModel(event.target.value); selectModel(event.target.value); setSaved(false); }} placeholder="gpt-5.2 or provider/model" />}{modelCatalog.isError ? <span className="mt-1 block text-[11px] text-amber-300">Live model check unavailable; using the cached model list.</span> : null}</Field>
          <Field label="Wire API"><select className={input} value={wireApi} onChange={(event) => setWireApi(event.target.value as "chat" | "responses")}><option value="chat">Chat Completions</option><option value="responses">Responses (Codex)</option></select></Field>
          <Field label="API Key"><input className={input} type="password" value={apiKey} onChange={(event) => { setApiKey(event.target.value); setSaved(false); }} placeholder="Leave blank to keep current key" /></Field>
        </div>
        {save.isError ? <p className="mt-2 text-xs text-red-400">{save.error.message}</p> : null}
        {saved ? <p className="mt-2 text-xs text-emerald-400">Saved. The key was written to the OS keychain.</p> : null}
        <div className="mt-3 flex flex-wrap items-center gap-2"><button type="button" disabled={save.isPending || !baseUrl.trim() || !model.trim()} onClick={() => save.mutate()} className="rounded-md bg-zinc-100 px-3 py-1.5 text-sm font-medium text-zinc-900 disabled:opacity-40">{save.isPending ? "Saving…" : "Save provider"}</button>{selectedProviderId ? <button type="button" disabled={modelCatalog.isFetching} onClick={() => void modelCatalog.refetch()} className="rounded-md border border-zinc-700 px-3 py-1.5 text-xs text-zinc-300 disabled:opacity-40">{modelCatalog.isFetching ? "Checking…" : "Refresh models"}</button> : null}{models.length ? <span className="text-xs text-zinc-500">{models.length} cached models</span> : null}</div>
      </div>

      <div className="rounded-lg border border-zinc-800 p-4"><h3 className="text-sm font-medium">Local profiles</h3><p className="mt-1 text-xs text-zinc-500">Read-only scan of CC Switch and the active Codex files. Importing copies metadata and a keychain reference.</p><div className="mt-3 space-y-3"><SourceList title="CC Switch" state={ccswitch} busy={importCcSwitch.isPending} onImport={(id) => importCcSwitch.mutate(id)} /><SourceList title="Codex config" state={codex} busy={importCodex.isPending} onImport={(id) => importCodex.mutate(id)} /></div>{importCcSwitch.isError ? <p className="mt-2 text-xs text-red-400">{String(importCcSwitch.error)}</p> : null}{importCodex.isError ? <p className="mt-2 text-xs text-red-400">{String(importCodex.error)}</p> : null}</div>
    </div>
  );
}

function Field({ label, className, children }: { label: string; className?: string; children: ReactNode }) { return <div className={className}><label className="mb-1 block text-xs text-zinc-400">{label}</label>{children}</div>; }

function SourceList({ title, state, busy, onImport }: { title: string; state: { data?: CcSwitchProviderCandidate[]; isLoading: boolean; isError: boolean }; busy: boolean; onImport: (id: string) => void }) {
  if (state.isLoading) return <p className="text-xs text-zinc-500">{title}: scanning…</p>;
  if (state.isError) return <p className="text-xs text-zinc-600">{title}: not found or unreadable</p>;
  if (!state.data?.length) return <p className="text-xs text-zinc-600">{title}: no profiles</p>;
  return <div><div className="mb-1 text-xs uppercase tracking-wide text-zinc-500">{title}</div><div className="space-y-1">{state.data.map((candidate) => <div key={candidate.id} className="flex items-center gap-2 rounded border border-zinc-800 px-2 py-1.5 text-xs"><span className="min-w-0 flex-1 truncate text-zinc-300">{candidate.name} · {candidate.model} <span className="text-zinc-600">· {candidate.models?.length ?? 0} models · {candidate.wireApi} · {candidate.hasApiKey ? "key" : "no key"}</span></span><button type="button" disabled={busy} onClick={() => onImport(candidate.id)} className="rounded border border-zinc-700 px-2 py-0.5 text-zinc-300 disabled:opacity-40">Import</button></div>)}</div></div>;
}

function Row({ label, value, wide = false }: { label: string; value: string; wide?: boolean }) { return <div className={wide ? "col-span-2" : undefined}><dt className="text-xs uppercase tracking-wide text-zinc-500">{label}</dt><dd className="break-all text-zinc-300">{value}</dd></div>; }
function unknown(shell: boolean): string { return shell ? "—" : "Unavailable"; }
function text(value: string | number | null | undefined, shell: boolean): string { return value === null || value === undefined ? unknown(shell) : String(value); }

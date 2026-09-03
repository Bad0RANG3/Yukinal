import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";
import { useState } from "react";

import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useAgentLogs, useAgentStatus, useCorePing } from "../../lib/runtime.js";

/**
 * Settings ▸ Runtime: agent 诊断（进程/协议/日志）与 AI Provider 配置。
 * 崩溃的 sidecar 必须在这里就地解释，而不是让用户去找终端。
 */
export function RuntimeSettings() {
  const [showLogs, setShowLogs] = useState(false);
  const core = useCorePing();
  const status = useAgentStatus();
  const logs = useAgentLogs(showLogs);
  const shell = isDesktopShell();

  return (
    <section className="max-w-2xl space-y-4">
      <h1 className="text-lg font-semibold">运行环境</h1>

      {!shell ? (
        <p className="rounded border border-amber-500/40 bg-amber-500/5 p-2 text-amber-200">
          纯浏览器预览：Rust 核心不可达，下面的值保持未知而不是造假。请用 <code>pnpm tauri dev</code> 启动。
        </p>
      ) : null}

      <dl className="grid grid-cols-2 gap-x-4 gap-y-2 text-sm">
        <Row label="原生核心" value={core.data ? `${core.data.version} (${core.data.os})` : unknown(shell)} />
        <Row label="Agent 状态" value={status.data ? (status.data.running ? "运行中" : "已停止") : unknown(shell)} />
        <Row label="Agent pid" value={text(status.data?.pid, shell)} />
        <Row label="协议" value={text(status.data?.protocolVersion, shell)} />
        <Row label="Agent 版本" value={text(status.data?.agentVersion, shell)} />
        <Row label="已注册工具" value={text(status.data?.toolCount, shell)} />
        <Row label="Sidecar 入口" value={text(status.data?.entry, shell)} wide />
        <Row
          label="Last exit"
          value={
            status.data?.lastExit
              ? `${status.data.lastExit.code ?? status.data.lastExit.signal ?? "未知"} @ ${status.data.lastExit.at}`
              : "无"
          }
          wide
        />
      </dl>

      <div>
        <button
          type="button"
          onClick={() => setShowLogs((current) => !current)}
          className="rounded border border-zinc-700 px-2 py-1 text-xs text-zinc-300"
        >
          {showLogs ? "隐藏 agent 日志" : "查看 agent 日志"}
        </button>
        {showLogs ? (
          <pre className="mt-2 max-h-64 overflow-auto rounded border border-zinc-800 bg-black/40 p-2 text-[11px] text-zinc-400">
            {logs.data?.lines.length ? logs.data.lines.join("\n") : "（暂未捕获输出）"}
          </pre>
        ) : null}
      </div>

      <ProviderSettings />
    </section>
  );
}

// ---------------------------------------------------------------------------
// AI Provider 配置：baseUrl / model / API key。key 只进 OS keychain。

function ProviderSettings() {
  const queryClient = useQueryClient();
  const [baseUrl, setBaseUrl] = useState("");
  const [model, setModel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [saved, setSaved] = useState(false);

  const providers = useQuery({
    queryKey: ["providers"],
    enabled: isDesktopShell(),
    queryFn: async () => (await callDesktop(IPC_COMMANDS.providerList, {})).providers,
  });

  const save = useMutation({
    mutationFn: () =>
      callDesktop(IPC_COMMANDS.providerSaveOpenai, {
        baseUrl: baseUrl.trim(),
        model: model.trim(),
        apiKey: apiKey.trim() || undefined,
      }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["providers"] });
      setSaved(true);
      setApiKey("");
    },
  });

  const input =
    "w-full rounded-md border border-zinc-800 bg-zinc-900 p-2 text-sm outline-none focus:border-zinc-600 placeholder:text-zinc-600";

  return (
    <div className="rounded-lg border border-zinc-800 p-4">
      <h2 className="mb-2 text-sm font-medium">AI Provider（OpenAI 兼容）</h2>

      {providers.data && providers.data.length > 0 ? (
        <ul className="mb-3 space-y-1 text-xs text-zinc-400">
          {providers.data.map((provider) => (
            <li key={provider.id}>
              {provider.label} · {provider.model} · {provider.enabled ? "已启用" : "停用"}
              {provider.apiKeyCredentialRef ? " · key 已存（keychain）" : " · 无 key"}
            </li>
          ))}
        </ul>
      ) : null}

      <div className="grid grid-cols-2 gap-3">
        <div className="col-span-2">
          <label className="mb-1 block text-xs text-zinc-400">Base URL</label>
          <input
            className={input}
            value={baseUrl}
            onChange={(event) => {
              setBaseUrl(event.target.value);
              setSaved(false);
            }}
            placeholder="https://openrouter.ai/api/v1（本机 Ollama: http://127.0.0.1:11434/v1）"
          />
        </div>
        <div>
          <label className="mb-1 block text-xs text-zinc-400">模型</label>
          <input
            className={input}
            value={model}
            onChange={(event) => {
              setModel(event.target.value);
              setSaved(false);
            }}
            placeholder="anthropic/claude-sonnet"
          />
        </div>
        <div>
          <label className="mb-1 block text-xs text-zinc-400">API Key</label>
          <input
            className={input}
            type="password"
            value={apiKey}
            onChange={(event) => {
              setApiKey(event.target.value);
              setSaved(false);
            }}
            placeholder="留空 = 本地端点 / 沿用旧 key"
          />
        </div>
      </div>

      {save.isError ? <p className="mt-2 text-xs text-red-400">{save.error.message}</p> : null}
      {saved ? <p className="mt-2 text-xs text-emerald-400">已保存（key 只进系统钥匙串）。</p> : null}

      <button
        type="button"
        disabled={save.isPending || !baseUrl.trim() || !model.trim()}
        onClick={() => save.mutate()}
        className="mt-3 rounded-md bg-zinc-100 px-3 py-1.5 text-sm font-medium text-zinc-900 disabled:opacity-40"
      >
        {save.isPending ? "保存中…" : "保存 Provider"}
      </button>
    </div>
  );
}

function Row({ label, value, wide = false }: { label: string; value: string; wide?: boolean }) {
  return (
    <div className={wide ? "col-span-2" : undefined}>
      <dt className="text-xs uppercase tracking-wide text-zinc-500">{label}</dt>
      <dd className="break-all text-zinc-300">{value}</dd>
    </div>
  );
}

function unknown(shell: boolean): string {
  return shell ? "…" : "不可用";
}

function text(value: string | number | null | undefined, shell: boolean): string {
  if (value === null || value === undefined) return unknown(shell);
  return String(value);
}
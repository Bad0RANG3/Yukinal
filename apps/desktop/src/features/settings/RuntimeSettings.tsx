import { useState } from "react";
import { isDesktopShell } from "../../lib/ipc.js";
import { useAgentLogs, useAgentStatus, useCorePing } from "../../lib/runtime.js";

/**
 * Settings ▸ Runtime: the diagnostics page behind the small strip in the sidebar.
 *
 * The sidecar log tail is shown because a crashed agent must be explainable in place
 * — not something the user has to go find in a terminal.
 */
export function RuntimeSettings() {
  const [showLogs, setShowLogs] = useState(false);
  const core = useCorePing();
  const status = useAgentStatus();
  const logs = useAgentLogs(showLogs);
  const shell = isDesktopShell();

  return (
    <section className="max-w-2xl space-y-4">
      <h1 className="text-lg font-semibold">Runtime</h1>

      {!shell ? (
        <p className="rounded border border-amber-500/40 bg-amber-500/5 p-2 text-amber-200">
          Running in a plain browser: the Rust core is not reachable, so every value below stays
          unknown instead of being faked. Launch with <code>pnpm tauri dev</code>.
        </p>
      ) : null}

      <dl className="grid grid-cols-2 gap-x-4 gap-y-2 text-sm">
        <Row label="Native core" value={core.data ? `${core.data.version} (${core.data.os})` : unknown(shell)} />
        <Row label="Agent status" value={status.data ? (status.data.running ? "running" : "stopped") : unknown(shell)} />
        <Row label="Agent pid" value={text(status.data?.pid, shell)} />
        <Row label="Protocol" value={text(status.data?.protocolVersion, shell)} />
        <Row label="Agent version" value={text(status.data?.agentVersion, shell)} />
        <Row label="Registered tools" value={text(status.data?.toolCount, shell)} />
        <Row label="Sidecar entry" value={text(status.data?.entry, shell)} wide />
        <Row
          label="Last exit"
          value={
            status.data?.lastExit
              ? `${status.data.lastExit.code ?? status.data.lastExit.signal ?? "unknown"} @ ${status.data.lastExit.at}`
              : "none"
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
          {showLogs ? "Hide agent log" : "Show agent log"}
        </button>
        {showLogs ? (
          <pre className="mt-2 max-h-64 overflow-auto rounded border border-zinc-800 bg-black/40 p-2 text-[11px] text-zinc-400">
            {logs.data?.lines.length ? logs.data.lines.join("\n") : "(no output captured yet)"}
          </pre>
        ) : null}
      </div>
    </section>
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
  return shell ? "…" : "unavailable";
}

function text(value: string | number | null | undefined, shell: boolean): string {
  if (value === null || value === undefined) return unknown(shell);
  return String(value);
}

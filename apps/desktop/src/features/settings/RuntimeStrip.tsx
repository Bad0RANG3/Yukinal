import { isDesktopShell } from "../../lib/ipc.js";
import { useAgentStatus, useCorePing, useKillAgent, useSpawnAgent, statusLabel } from "../../lib/runtime.js";

/**
 * Runtime strip: what native processes are alive, reported by Rust.
 *
 * Deliberately small and out of the way — wants users thinking about servers
 * and health, not about pids. It stays visible because "is the agent even running"
 * must never be a guess.
 */
export function RuntimeStrip() {
  const core = useCorePing();
  const status = useAgentStatus();
  const spawn = useSpawnAgent();
  const kill = useKillAgent();

  const shell = isDesktopShell();
  const running = status.data?.running === true;

  return (
    <div className="mt-auto border-t border-zinc-800 px-2 py-2 text-[11px] leading-relaxed text-zinc-500">
      <div className="flex items-center justify-between gap-2">
        <span className="truncate">
          <span className={running ? "text-emerald-400" : "text-zinc-500"}>
            {shell ? (running ? "●" : "○") : "○"}
          </span>{" "}
          {statusLabel(status.data, shell)}
        </span>
      </div>

      <div className="mt-1 flex items-center gap-2">
        <button
          type="button"
          disabled={!shell || running || spawn.isPending}
          onClick={() => spawn.mutate()}
          className="rounded border border-zinc-700 px-1.5 py-0.5 text-zinc-300 disabled:opacity-40"
        >
          {spawn.isPending ? "starting…" : "Start agent"}
        </button>
        <button
          type="button"
          disabled={!shell || !running || kill.isPending}
          onClick={() => kill.mutate()}
          className="rounded border border-zinc-700 px-1.5 py-0.5 text-zinc-300 disabled:opacity-40"
        >
          {kill.isPending ? "stopping…" : "Stop"}
        </button>
      </div>

      {spawn.isError ? <p className="mt-1 break-words text-red-400">{spawn.error.message}</p> : null}
      {status.isError ? <p className="mt-1 break-words text-red-400">{status.error.message}</p> : null}

      <p className="mt-1 truncate">
        {shell && core.data ? `core ${core.data.version} · ${core.data.os}` : "core: unavailable"}
      </p>
    </div>
  );
}

/**
 * Runtime strip：原生进程是否活着，由 Rust 上报。刻意小而轻 —— 让用户想服务器和
 * 健康，而不是 pid；但它必须常驻可见：“agent 到底跑没跑”不能靠猜。
 */

import { isDesktopShell } from "../../lib/ipc.js";
import { useAgentStatus, useCorePing, useKillAgent, useSpawnAgent, statusLabel } from "../../lib/runtime.js";

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
          {spawn.isPending ? "启动中…" : "启动 agent"}
        </button>
        <button
          type="button"
          disabled={!shell || !running || kill.isPending}
          onClick={() => kill.mutate()}
          className="rounded border border-zinc-700 px-1.5 py-0.5 text-zinc-300 disabled:opacity-40"
        >
          {kill.isPending ? "停止中…" : "停止"}
        </button>
      </div>

      {spawn.isError ? <p className="mt-1 break-words text-red-400">{spawn.error.message}</p> : null}
      {status.isError ? <p className="mt-1 break-words text-red-400">{status.error.message}</p> : null}

      <p className="mt-1 truncate">
        {shell && core.data ? `core ${core.data.version} · ${core.data.os}` : "core：不可用（浏览器环境）"}
      </p>
    </div>
  );
}
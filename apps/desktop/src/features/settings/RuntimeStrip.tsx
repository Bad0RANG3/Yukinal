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
    <div className="runtime-strip">
      <div className="runtime-strip-title">
        <span className={`runtime-dot ${running ? "runtime-dot-running" : ""}`} />
        <span className="runtime-strip-label">运行时</span>
        <span className="runtime-strip-state">{running ? "在线" : shell ? "已停止" : "预览"}</span>
      </div>
      <p className="runtime-strip-detail">{statusLabel(status.data, shell)}</p>

      <div className="runtime-actions">
        <button
          type="button"
          disabled={!shell || running || spawn.isPending}
          onClick={() => spawn.mutate()}
          className="runtime-action"
        >
          {spawn.isPending ? "启动中…" : "启动 agent"}
        </button>
        <button
          type="button"
          disabled={!shell || !running || kill.isPending}
          onClick={() => kill.mutate()}
          className="runtime-action"
        >
          {kill.isPending ? "停止中…" : "停止"}
        </button>
      </div>

      {spawn.isError ? <p className="runtime-error">{spawn.error.message}</p> : null}
      {status.isError ? <p className="runtime-error">{status.error.message}</p> : null}

      <p className="runtime-core">
        {shell && core.data ? `core ${core.data.version} · ${core.data.os}` : "core：不可用（浏览器环境）"}
      </p>
    </div>
  );
}

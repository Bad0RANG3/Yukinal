/**
 * Terminal pane: xterm.js over the real Rust PTY chain.
 *
 * Flow: xterm onData → `terminal_write` → Rust PTY Manager → SSH channel → remote PTY;
 * remote bytes → `terminal.data` event → xterm.write. Nothing is faked; if there is
 * no server yet (no SQLite row), the pane shows the honest empty state instead of a
 * pretend terminal.
 */

import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useEffect, useRef } from "react";

import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { IPC_COMMANDS } from "@yukinal/shared";

export function TerminalPane() {
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!selectedServerId || !isDesktopShell()) return;

    const container = containerRef.current;
    if (!container) return;

    const term = new Terminal({
      cursorBlink: true,
      fontSize: 13,
      fontFamily: "Menlo, Consolas, 'Courier New', monospace",
      scrollback: 10_000,
      theme: { background: "#09090b" },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(container);
    fit.fit();

    const unlisteners: UnlistenFn[] = [];
    let sessionId: string | null = null;
    let disposed = false;

    // Remote bytes → xterm. The event carries the session id so one pane can't
    // write foreign output if a second terminal is open.
    listen<{ terminalSessionId: string; data: string }>("terminal.data", (event) => {
      if (disposed || (sessionId !== null && event.payload.terminalSessionId !== sessionId)) return;
      term.write(event.payload.data);
    }).then((unlisten) => unlisteners.push(unlisten));

    listen<{ terminalSessionId: string }>("terminal.closed", (event) => {
      if (disposed || event.payload.terminalSessionId !== sessionId) return;
      term.write("\r\n\x1b[1;31m[session closed]\x1b[0m\r\n");
    }).then((unlisten) => unlisteners.push(unlisten));

    // Open the PTY through the trusted chain.
    const rows = term.rows;
    callDesktop(IPC_COMMANDS.terminalOpen, {
      serverId: selectedServerId,
      cols: term.cols,
      rows: rows > 0 ? rows : 24,
    })
      .then(({ terminalSessionId }) => {
        sessionId = terminalSessionId;
        // Terminal emits its current size after open; bidirectional wiring starts
        // from here so a resize before this point is not lost.
        const io = term.onData((data) => {
          if (sessionId !== null && isDesktopShell()) {
            callDesktop(IPC_COMMANDS.terminalWrite, {
              terminalSessionId: sessionId,
              data,
            }).catch(() => {});
          }
        });
        const resize = term.onResize(({ cols, rows }) => {
          if (sessionId !== null && isDesktopShell()) {
            callDesktop(IPC_COMMANDS.terminalResize, {
              terminalSessionId: sessionId,
              cols,
              rows,
            }).catch(() => {});
          }
        });
        unlisteners.push(async () => {
          io.dispose();
          resize.dispose();
        });
      })
      .catch((error) => {
        term.write(`\r\n\x1b[1;31mterminal_open failed:\x1b[0m ${String(error)}\r\n`);
      });
    term.focus();

    return () => {
      disposed = true;
      unlisteners.forEach((unlisten) => unlisten());
      if (sessionId !== null) {
        callDesktop(IPC_COMMANDS.terminalClose, { terminalSessionId: sessionId }).catch(() => {});
      }
      term.dispose();
    };
  }, [selectedServerId]);

  if (!selectedServerId) {
    return (
      <div className="flex h-full min-h-[240px] items-center justify-center rounded-lg border border-dashed border-zinc-800 text-sm text-zinc-500">
        需要一个服务器才能打开终端（Server ▸ add）。
      </div>
    );
  }

  return <div ref={containerRef} className="h-full min-h-[240px] w-full overflow-hidden" />;
}
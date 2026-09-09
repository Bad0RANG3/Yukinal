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
import { IPC_COMMANDS } from "@yukinal/shared";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";

import { Icon } from "../../components/Icon.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { usePreferencesStore, type TerminalFont } from "../../stores/preferences-store.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";

const TERMINAL_THEME = {
  background: "#080808",
  foreground: "#d9d9d9",
  cursor: "#d0d0d0",
  cursorAccent: "#080808",
  selectionBackground: "#454545",
  selectionForeground: "#f3f3f3",
  black: "#161616",
  brightBlack: "#686868",
  red: "#bdbdbd",
  brightRed: "#eeeeee",
  green: "#c4c4c4",
  brightGreen: "#f0f0f0",
  yellow: "#b5b5b5",
  brightYellow: "#e2e2e2",
  blue: "#a7a7a7",
  brightBlue: "#d3d3d3",
  magenta: "#b0b0b0",
  brightMagenta: "#e0e0e0",
  cyan: "#aaaaaa",
  brightCyan: "#d5d5d5",
  white: "#c5c5c5",
  brightWhite: "#f1f1f1",
};

export function TerminalPane({ active }: { active: boolean }) {
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const containerRef = useRef<HTMLDivElement>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const preferences = usePreferencesStore();
  const [opened, setOpened] = useState(active);
  const activeRef = useRef(active);
  activeRef.current = active;

  useEffect(() => { if (active) setOpened(true); }, [active]);

  useEffect(() => {
    const term = terminalRef.current;
    const fit = fitRef.current;
    if (!term || !fit) return;
    term.options.fontFamily = terminalFontFamily(preferences.terminalFont);
    term.options.fontSize = preferences.terminalFontSize;
    term.options.lineHeight = preferences.terminalLineHeight;
    term.options.cursorBlink = preferences.terminalCursorBlink;
    const frame = requestAnimationFrame(() => { if (activeRef.current && terminalRef.current === term) fit.fit(); });
    return () => cancelAnimationFrame(frame);
  }, [preferences.terminalCursorBlink, preferences.terminalFont, preferences.terminalFontSize, preferences.terminalLineHeight]);

  useEffect(() => {
    if (!opened || !selectedServerId || !isDesktopShell()) return;

    const container = containerRef.current;
    if (!container) return;

    const initialPreferences = usePreferencesStore.getState();
    const term = new Terminal({
      cursorBlink: initialPreferences.terminalCursorBlink,
      cursorStyle: "bar",
      fontSize: initialPreferences.terminalFontSize,
      fontFamily: terminalFontFamily(initialPreferences.terminalFont),
      lineHeight: initialPreferences.terminalLineHeight,
      scrollback: 10_000,
      convertEol: false,
      theme: TERMINAL_THEME,
    });
    const fit = new FitAddon();
    terminalRef.current = term;
    fitRef.current = fit;
    term.loadAddon(fit);
    term.open(container);
    fit.fit();

    const unlisteners: UnlistenFn[] = [];
    let sessionId: string | null = null;
    let disposed = false;
    const resizeObserver = new ResizeObserver(() => {
      requestAnimationFrame(() => {
        if (!disposed && activeRef.current) fit.fit();
      });
    });
    resizeObserver.observe(container);

    // Remote bytes → xterm. The event carries the session id so one pane can't
    // write foreign output if a second terminal is open.
    void listen<{ terminalSessionId: string; data: string }>("terminal.data", (event) => {
      if (disposed || (sessionId !== null && event.payload.terminalSessionId !== sessionId)) return;
      term.write(event.payload.data);
    }).then((unlisten) => {
      if (disposed) unlisten();
      else unlisteners.push(unlisten);
    });

    void listen<{ terminalSessionId: string; exitCode: number | null }>("terminal.closed", (event) => {
      if (disposed || event.payload.terminalSessionId !== sessionId) return;
      const suffix = event.payload.exitCode === null ? "" : ` (exit ${event.payload.exitCode})`;
      term.write(`\r\n\x1b[1;31m[会话已关闭${suffix}]\x1b[0m\r\n`);
    }).then((unlisten) => {
      if (disposed) unlisten();
      else unlisteners.push(unlisten);
    });

    // Open the PTY through the trusted chain.
    const rows = term.rows;
    void callDesktop(IPC_COMMANDS.terminalOpen, {
      serverId: selectedServerId,
      cols: term.cols,
      rows: rows > 0 ? rows : 24,
    })
      .then(({ terminalSessionId }) => {
        if (disposed) {
          void callDesktop(IPC_COMMANDS.terminalClose, { terminalSessionId }).catch(() => {});
          return;
        }
        sessionId = terminalSessionId;
        void callDesktop(IPC_COMMANDS.terminalResize, { terminalSessionId, cols: term.cols, rows: term.rows }).catch(() => {});
        // Terminal emits its current size after open; bidirectional wiring starts
        // from here so a resize before this point is not lost.
        const io = term.onData((data) => {
          if (sessionId !== null && isDesktopShell()) {
            void callDesktop(IPC_COMMANDS.terminalWrite, {
              terminalSessionId: sessionId,
              data,
            }).catch(() => {});
          }
        });
        const resize = term.onResize(({ cols, rows: nextRows }) => {
          if (sessionId !== null && isDesktopShell()) {
            void callDesktop(IPC_COMMANDS.terminalResize, {
              terminalSessionId: sessionId,
              cols,
              rows: nextRows,
            }).catch(() => {});
          }
        });
        unlisteners.push(() => {
          io.dispose();
          resize.dispose();
        });
      })
      .catch((error) => {
        if (!disposed) term.write(`\r\n\x1b[1;31m终端打开失败：\x1b[0m${String(error)}\r\n`);
      });
    if (activeRef.current) term.focus();

    return () => {
      disposed = true;
      resizeObserver.disconnect();
      unlisteners.splice(0).forEach((unlisten) => unlisten());
      if (sessionId !== null) {
        void callDesktop(IPC_COMMANDS.terminalClose, { terminalSessionId: sessionId }).catch(() => {});
      }
      terminalRef.current = null;
      fitRef.current = null;
      term.dispose();
    };
  }, [selectedServerId, opened]);

  useEffect(() => {
    if (!active) return;
    fitRef.current?.fit();
    terminalRef.current?.focus();
  }, [active, opened]);

  if (!opened) return null;

  if (!selectedServerId) {
    return (
      <div className="terminal-empty">
        <Icon name="terminal" size={22} />
        <strong>选择服务器后打开终端</strong>
        <span>先在左侧添加或选择一台服务器。</span>
      </div>
    );
  }

  return (
    <section className="terminal-page">
      <div className="terminal-toolbar">
        <div className="section-heading-inline"><Icon name="terminal" size={16} /><div><p className="eyebrow">SSH</p><h2>终端</h2></div></div>
        <span className="terminal-toolbar-note">{preferences.terminalFont === "jetbrains-mono" ? "JetBrains Mono" : preferences.terminalFont === "jetbrains-nerd-mono" ? "JetBrains Mono Nerd" : "JetBrains Mono NL"} · {preferences.terminalFontSize}px</span>
      </div>
      <div ref={containerRef} className="terminal-shell" aria-label="SSH 终端" />
    </section>
  );
}

function terminalFontFamily(font: TerminalFont): string {
  if (font === "jetbrains-nerd-mono") return '"JetBrains Mono Nerd", "JetBrains Mono NL", "JetBrains Mono", "Cascadia Mono", Consolas, "Microsoft YaHei UI", monospace';
  if (font === "jetbrains-mono") return '"JetBrains Mono", "JetBrains Mono NL", "Cascadia Mono", Consolas, "Microsoft YaHei UI", monospace';
  return '"JetBrains Mono NL", "JetBrains Mono", "Cascadia Mono", Consolas, "Microsoft YaHei UI", monospace';
}

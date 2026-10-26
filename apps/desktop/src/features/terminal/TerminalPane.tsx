/**
 * Terminal pane: xterm.js over the real C++ host and SSH PTY chain.
 *
 * Flow: xterm onData → `terminal_write` → C++ host → SSH channel → remote PTY;
 * remote bytes → `terminal.data` event → xterm.write. Nothing is faked; if there is
 * no server yet (no SQLite row), the pane shows the honest empty state instead of a
 * pretend terminal.
 */

import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { IPC_COMMANDS } from "@yukinal/shared";
import { useEffect, useRef, useState } from "react";

import { errorMessage } from "../../lib/format.js";
import { TERMINAL_FONT_FAMILY, TERMINAL_FONT_LABEL } from "../../lib/labels.js";
import { Icon } from "../../components/Icon.js";
import { callDesktop, isDesktopShell, subscribeDesktop } from "../../lib/ipc.js";
import { usePreferencesStore, type TerminalFont } from "../../stores/preferences-store.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { terminalClosedNotice } from "./terminal-exit.js";
import { pasteFromClipboard, TerminalPasteError } from "./terminal-paste.js";

interface PasteSession {
  terminal: Terminal;
  serverId: string;
  sessionId: string;
}

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

export function TerminalPane({ active, serverId }: { active: boolean; serverId?: string | null }) {
  const workspaceServerId = useWorkspaceStore((state) => state.selectedServerId);
  const selectedServerId = serverId === undefined ? workspaceServerId : serverId;
  const containerRef = useRef<HTMLDivElement>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const pasteSessionRef = useRef<PasteSession | null>(null);
  const pasteAttemptRef = useRef(0);
  const pasteReadingRef = useRef(false);
  const selectedServerRef = useRef(selectedServerId);
  selectedServerRef.current = selectedServerId;
  const preferences = usePreferencesStore();
  const [opened, setOpened] = useState(active);
  const [pasteSession, setPasteSession] = useState<PasteSession | null>(null);
  const [pasteReading, setPasteReading] = useState(false);
  const [pasteError, setPasteError] = useState<string | null>(null);
  /**
   * First write/resize failure for the current session, or null.
   *
   * `terminal_write` used to be the one IPC call whose failure was discarded, so
   * a dead session kept the pane looking interactive while keystrokes went
   * nowhere. Only the first failure is kept, so a dead session cannot queue one
   * banner per keystroke.
   */
  const [ioError, setIoError] = useState<string | null>(null);
  const [reconnectNonce, setReconnectNonce] = useState(0);
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

    setIoError(null);
    setPasteSession(null);
    setPasteError(null);
    setPasteReading(false);
    /** Keep the first failure only; a functional update is a no-op once set. */
    const reportIoError = (error: unknown): void => {
      if (disposed) return;
      invalidatePasteSession();
      setIoError((current) => current ?? (errorMessage(error)));
    };

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

    const unlisteners: Array<() => void> = [];
    let sessionId: string | null = null;
    let disposed = false;
    const invalidatePasteSession = () => {
      if (pasteSessionRef.current?.terminal !== term) return;
      pasteSessionRef.current = null;
      pasteAttemptRef.current++;
      pasteReadingRef.current = false;
      setPasteSession(null);
      setPasteReading(false);
    };
    const pendingData: Array<{ terminalSessionId: string; data: string }> = [];
    let pendingDataChars = 0;
    const MAX_PENDING_DATA_CHARS = 256_000;
    const resizeObserver = new ResizeObserver(() => {
      requestAnimationFrame(() => {
        if (!disposed && activeRef.current) fit.fit();
      });
    });
    resizeObserver.observe(container);

    // Remote bytes → xterm. The event carries the session id so one pane can't
    // write foreign output if a second terminal is open. The short pre-response
    // window is buffered by id so the first prompt is not lost, then foreign
    // sessions are discarded once this pane knows its own id.
    //
    // Subscribed through `listenDesktop`, so the payload is parsed against the
    // shared contract before it reaches xterm: this data is read off a remote
    // host, and the previous `listen<T>` cast validated nothing.
    // Subscribed through `subscribeDesktop`, so the payload is parsed against the
    // shared contract before it reaches xterm: this data is read off a remote host,
    // and the previous `listen<T>` cast validated nothing. The subscribe/unsubscribe
    // race is handled inside that helper —— 这里不再需要手写 `disposed` 标志。
    unlisteners.push(
      subscribeDesktop("terminal.data", (payload) => {
        if (disposed) return;
        if (sessionId === null) {
          const data = payload.data;
          while (pendingDataChars + data.length > MAX_PENDING_DATA_CHARS && pendingData.length > 0) {
            const removed = pendingData.shift();
            pendingDataChars -= removed?.data.length ?? 0;
          }
          if (data.length <= MAX_PENDING_DATA_CHARS) {
            pendingData.push(payload);
            pendingDataChars += data.length;
          }
          return;
        }
        if (payload.terminalSessionId !== sessionId) return;
        term.write(payload.data);
      }).stop,
    );

    unlisteners.push(
      subscribeDesktop("terminal.closed", (payload) => {
        if (disposed || payload.terminalSessionId !== sessionId) return;
        invalidatePasteSession();
        // Unknown, zero and non-zero exit statuses must read differently: a bare
        // "closed" reads as success, which is the false-green the native fix exists
        // to prevent. The sentence lives in `terminal-exit.ts` so it can be tested.
        term.write(terminalClosedNotice(payload.exitCode));
      }).stop,
    );

    unlisteners.push(
      // 宿主广播队列溢出时会明确告知丢帧，而不是静默停止转发（性能 P0）。
      subscribeDesktop("terminal.output_lost", (payload) => {
        if (disposed) return;
        term.write(`\r\n\x1b[1;33m[输出速率过高，${payload.missedEvents} 个终端帧已丢失]\x1b[0m\r\n`);
      }).stop,
    );

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
        for (const event of pendingData) {
          if (event.terminalSessionId === sessionId) term.write(event.data);
        }
        pendingData.length = 0;
        pendingDataChars = 0;
        void callDesktop(IPC_COMMANDS.terminalResize, { terminalSessionId, cols: term.cols, rows: term.rows }).catch(reportIoError);
        // Terminal emits its current size after open; bidirectional wiring starts
        // from here so a resize before this point is not lost.
        const io = term.onData((data) => {
          if (!disposed && sessionId !== null && isDesktopShell()) {
            void callDesktop(IPC_COMMANDS.terminalWrite, {
              terminalSessionId: sessionId,
              data,
            }).catch(reportIoError);
          }
        });
        const resize = term.onResize(({ cols, rows: nextRows }) => {
          if (!disposed && sessionId !== null && isDesktopShell()) {
            void callDesktop(IPC_COMMANDS.terminalResize, {
              terminalSessionId: sessionId,
              cols,
              rows: nextRows,
            }).catch(reportIoError);
          }
        });
        unlisteners.push(() => {
          io.dispose();
          resize.dispose();
        });
        const readySession = { terminal: term, serverId: selectedServerId, sessionId: terminalSessionId };
        pasteSessionRef.current = readySession;
        setPasteSession(readySession);
      })
      .catch(reportIoError);
    if (activeRef.current) term.focus();

    return () => {
      disposed = true;
      if (pasteSessionRef.current?.terminal === term) {
        pasteSessionRef.current = null;
        pasteAttemptRef.current++;
        pasteReadingRef.current = false;
      }
      resizeObserver.disconnect();
      unlisteners.splice(0).forEach((unlisten) => unlisten());
      if (sessionId !== null) {
        void callDesktop(IPC_COMMANDS.terminalClose, { terminalSessionId: sessionId }).catch(() => {});
      }
      terminalRef.current = null;
      fitRef.current = null;
      term.dispose();
    };
  }, [selectedServerId, opened, reconnectNonce]);

  useEffect(() => {
    if (!active) return;
    fitRef.current?.fit();
    terminalRef.current?.focus();
  }, [active, opened]);

  const pasteClipboard = () => {
    const session = pasteSessionRef.current;
    if (!session || session.serverId !== selectedServerId || !activeRef.current || pasteReadingRef.current || ioError !== null) return;
    const attempt = ++pasteAttemptRef.current;
    const isCurrent = () => pasteAttemptRef.current === attempt && pasteSessionRef.current === session
      && terminalRef.current === session.terminal && selectedServerRef.current === session.serverId && activeRef.current;
    pasteReadingRef.current = true;
    setPasteReading(true);
    setPasteError(null);
    void pasteFromClipboard(session.terminal, () => {
      if (!navigator.clipboard?.readText) return Promise.reject(new Error("Clipboard unavailable"));
      return navigator.clipboard.readText();
    }, isCurrent)
      .catch((error: unknown) => {
        if (isCurrent()) setPasteError(error instanceof TerminalPasteError ? error.message : "终端暂时无法粘贴，请再次尝试。");
      })
      .finally(() => {
        if (pasteAttemptRef.current !== attempt) return;
        pasteReadingRef.current = false;
        setPasteReading(false);
      });
  };
  const pasteEnabled = active && pasteSession?.serverId === selectedServerId && ioError === null && !pasteReading;

  if (!opened) return null;

  if (!selectedServerId) {
    return (
      <div className="terminal-empty">
        <Icon name="terminal" size="xl" />
        <strong>选择服务器后打开终端</strong>
        <span>先在左侧添加或选择一台服务器。</span>
      </div>
    );
  }

  return (
    <section className="terminal-page">
      <div className="terminal-toolbar">
        <div className="section-heading-inline"><Icon name="terminal" size="md" /><span>SSH</span></div>
        <span className="terminal-toolbar-note">{TERMINAL_FONT_LABEL[preferences.terminalFont]} · {preferences.terminalFontSize}px</span>
        <button type="button" className="button-secondary terminal-paste-button" disabled={!pasteEnabled} onClick={pasteClipboard} aria-label="粘贴到终端" aria-busy={pasteReading} title="粘贴剪贴板文字，不补回车">
          <Icon name="copy" size="sm" />{pasteReading ? "正在读取…" : "粘贴到终端"}
        </button>
      </div>
      {pasteError !== null ? <div className="terminal-paste-error" role="alert">
        <Icon name="warning" size="sm" /><span>{pasteError}</span>
        <button type="button" className="button-secondary" onClick={pasteClipboard} disabled={!pasteEnabled}>重试粘贴</button>
        <button type="button" className="icon-button" onClick={() => setPasteError(null)} aria-label="关闭粘贴提示"><Icon name="close" size="sm" /></button>
      </div> : null}
      {ioError !== null && (
        <div className="terminal-io-error" role="alert">
          <Icon name="warning" size="sm" />
          <span>终端连接或数据传输失败：{ioError}</span>
          <button
            type="button"
            className="button-secondary"
            onClick={() => { setIoError(null); setReconnectNonce((nonce) => nonce + 1); }}
          >
            重新连接
          </button>
        </div>
      )}
      <div ref={containerRef} className="terminal-shell" aria-label="SSH 终端" />
    </section>
  );
}

function terminalFontFamily(font: TerminalFont): string {
  // 栈必须是字面量：xterm 自己量字符宽度，不做 CSS 变量替换，传 `var(--…)` 会量错。
  return TERMINAL_FONT_FAMILY[font];
}

/**
 * What a closed terminal says about the remote exit status.
 *
 * The host reports three different things and they must not collapse into one
 * sentence: an integer status, an explicit `null`, and no frame at all. `null`
 * means *unknown* — the service either said so or died before saying anything —
 * and rendering that as a plain "session closed" reads as success, which is the
 * exact false-green that `terminal_exit.hpp` exists to prevent.
 *
 * Kept as a pure function because nothing else can assert on it: the notice is
 * written straight into xterm, which no render-to-string test can observe.
 */

/** The reason sentence shown after a closed terminal. */
export function terminalExitLabel(exitCode: number | null): string {
  if (exitCode === null) return "远端退出状态未知";
  if (exitCode === 0) return "正常结束 (exit 0)";
  return `异常结束 (exit ${exitCode})`;
}

/**
 * The full line written into the terminal, with xterm colour escapes.
 *
 * `red` is deliberate for both the unknown and the non-zero case: the only state
 * that may read as neutral is a confirmed zero, and even that is coloured so the
 * boundary is visible in scrollback.
 */
export function terminalClosedNotice(exitCode: number | null): string {
  return `\r\n\x1b[1;31m[会话已关闭 · ${terminalExitLabel(exitCode)}]\x1b[0m\r\n`;
}

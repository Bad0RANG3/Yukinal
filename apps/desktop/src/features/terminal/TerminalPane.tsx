/**
 * Terminal pane.
 *
 * xterm.js is mounted here once `yukinal-terminal` exists. Deliberately NOT
 * implemented in the shell: a terminal that fakes a connection would violate the
 * Principle 3 promise that everything shown actually happened.
 */
export function TerminalPane() {
  return (
    <div className="flex h-full min-h-[240px] items-center justify-center rounded-lg border border-dashed border-zinc-800 text-sm text-zinc-500">
      Terminal (xterm.js + PTY over SSH) is not implemented yet. Overview stays the default page — Principle 1.
    </div>
  );
}

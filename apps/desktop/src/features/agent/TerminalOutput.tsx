import type { StyledRun } from "./terminal-colour.js";

/**
 * Render tool output the operator explicitly expanded.
 *
 * Text stays text: each run becomes its own `<span>`, so React keeps escaping it
 * and no output byte can become markup. The parser that produced these runs has
 * already stripped every control character and every non-SGR escape, so a hostile
 * server cannot move a cursor, retitle the window or write the clipboard through
 * a log line.
 */
export function TerminalOutput({ runs, label }: { runs: StyledRun[]; label?: string }) {
  if (runs.length === 0) return null;
  return <pre className="tool-result-copy terminal-output" aria-label={label}>
    {runs.map((run, index) => run.tone === "plain"
      ? run.text
      : <span key={index} className={`terminal-run terminal-run-${run.tone}`}>{run.text}</span>)}
  </pre>;
}

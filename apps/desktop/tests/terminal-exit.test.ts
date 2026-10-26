/**
 * The closed-terminal notice must distinguish "unknown" from "success".
 *
 * This is the downstream half of the native `terminal.closed` fix: the host now
 * sends the remote exit code (or an explicit `null`), but if the pane printed the
 * same sentence for both, the operator would still read a failed backup as a
 * finished one. The pane writes into xterm, so the sentence is a pure function and
 * this is where it is pinned.
 */

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { terminalClosedNotice, terminalExitLabel } from "../src/features/terminal/terminal-exit.js";

test("unknown, zero and non-zero exit statuses read as three different outcomes", () => {
  assert.equal(terminalExitLabel(null), "远端退出状态未知");
  assert.equal(terminalExitLabel(0), "正常结束 (exit 0)");
  assert.equal(terminalExitLabel(7), "异常结束 (exit 7)");
  assert.equal(terminalExitLabel(130), "异常结束 (exit 130)");
  // The three sentences are mutually distinct: a copy-paste that made two of them
  // equal would pass a per-value assertion but fail this one.
  assert.equal(new Set([terminalExitLabel(null), terminalExitLabel(0), terminalExitLabel(7)]).size, 3);
});

test("an unknown status is never rendered as a number", () => {
  const notice = terminalClosedNotice(null);
  assert.match(notice, /会话已关闭/);
  assert.match(notice, /未知/);
  assert.doesNotMatch(notice, /exit 0|exit null|exit undefined/);
});

test("the notice carries the exit code and stays a single terminal line", () => {
  const notice = terminalClosedNotice(7);
  assert.match(notice, /exit 7/);
  // Exactly the leading and trailing newline, plus escapes: no inner newline that
  // would break the prompt line the user is about to type on.
  assert.equal(notice.trimEnd().split("\n").length, 2);
  assert.ok(notice.startsWith("\r\n"), "the notice must start on its own line");
  assert.ok(notice.endsWith("\r\n"), "the notice must not swallow the next prompt");
});

test("the terminal pane uses this function instead of formatting inline", () => {
  // Guard against the formatting drifting back into JSX: a second copy is how the
  // "unknown == success" bug would return without any test failing.
  const source = readFileSync(new URL("../src/features/terminal/TerminalPane.tsx", import.meta.url), "utf8");
  assert.match(source, /terminalClosedNotice\(payload\.exitCode\)/);
  assert.doesNotMatch(source, /exit \$\{payload\.exitCode\}/);
});

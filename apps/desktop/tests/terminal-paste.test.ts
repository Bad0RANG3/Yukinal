import assert from "node:assert/strict";
import test from "node:test";
import {
  pasteFromClipboard,
  TerminalPasteError,
  TERMINAL_INPUT_BYTE_LIMIT,
  validateTerminalPaste,
  type TerminalPasteTarget,
} from "../src/features/terminal/terminal-paste.js";

function terminal(bracketedPasteMode = false, ignoreBracketedPasteMode = false) {
  const pasted: string[] = [];
  let focused = 0;
  const target: TerminalPasteTarget = {
    modes: { bracketedPasteMode }, options: { ignoreBracketedPasteMode },
    paste: (text) => { pasted.push(text); },
    focus: () => { focused++; },
  };
  return { target, pasted, get focused() { return focused; } };
}

test("a click-triggered read goes straight to xterm paste without adding Enter or changing clipboard text", async () => {
  const term = terminal(true);
  const text = "printf '中文'\nnext\r\nlast";
  let reads = 0;
  const pending = pasteFromClipboard(term.target, async () => { reads++; return text; }, () => true);
  assert.equal(reads, 1, "read starts in the user gesture's synchronous call stack");
  assert.equal(await pending, "pasted");
  assert.deepEqual(term.pasted, [text]);
  assert.equal(term.focused, 1);
  assert.equal(reads, 1);
});

test("the byte limit counts UTF-8 and bracketed-paste markers instead of truncating input", async () => {
  const limit = TERMINAL_INPUT_BYTE_LIMIT;
  assert.doesNotThrow(() => validateTerminalPaste("a".repeat(limit), false));
  assert.doesNotThrow(() => validateTerminalPaste("a".repeat(limit - 12), true));
  assert.doesNotThrow(() => validateTerminalPaste("中".repeat(Math.floor((limit - 12) / 3)), true));
  for (const [text, bracketed] of [
    ["a".repeat(limit + 1), false],
    ["a".repeat(limit - 11), true],
    ["中".repeat(Math.ceil(limit / 3)), false],
    ["😀".repeat(limit / 4), true],
  ] as const) assert.throws(() => validateTerminalPaste(text, bracketed), /64 KiB/);

  const term = terminal(true);
  await assert.rejects(pasteFromClipboard(term.target, async () => "a".repeat(limit), () => true), /64 KiB/);
  assert.deepEqual(term.pasted, []);
  assert.equal(term.focused, 0);
});

test("the terminal's current paste mode is used after the asynchronous clipboard read", async () => {
  const term = terminal(false);
  let resolve!: (text: string) => void;
  const pending = pasteFromClipboard(term.target, () => new Promise<string>((done) => { resolve = done; }), () => true);
  Object.assign(term.target.modes, { bracketedPasteMode: true });
  resolve("a".repeat(TERMINAL_INPUT_BYTE_LIMIT));
  await assert.rejects(pending, /64 KiB/);
  assert.deepEqual(term.pasted, []);

  const ignored = terminal(true, true);
  await pasteFromClipboard(ignored.target, async () => "a".repeat(TERMINAL_INPUT_BYTE_LIMIT), () => true);
  assert.equal(ignored.pasted[0]?.length, TERMINAL_INPUT_BYTE_LIMIT);
});

test("session changes or disposal during a clipboard read cannot paste or focus an old terminal", async () => {
  const term = terminal();
  let current = true;
  let resolve!: (text: string) => void;
  const pending = pasteFromClipboard(term.target, () => new Promise<string>((done) => { resolve = done; }), () => current);
  current = false;
  resolve("old session input");
  assert.equal(await pending, "stale");
  assert.deepEqual(term.pasted, []);
  assert.equal(term.focused, 0);
  let reads = 0;
  assert.equal(await pasteFromClipboard(term.target, async () => { reads++; return "text"; }, () => false), "stale");
  assert.equal(reads, 0);
});

test("clipboard permission failures are recoverable and never expose error contents", async () => {
  const term = terminal();
  await assert.rejects(pasteFromClipboard(term.target, async () => { throw new Error("private clipboard detail"); }, () => true), (error) => {
    assert.ok(error instanceof TerminalPasteError);
    assert.match(error.message, /允许剪贴板访问后重试/);
    assert.ok(!error.message.includes("private"));
    return true;
  });
  assert.deepEqual(term.pasted, []);
  assert.equal(await pasteFromClipboard(term.target, async () => "id", () => true), "pasted");
  assert.deepEqual(term.pasted, ["id"]);
});

test("a failed read belonging to a previous session is silently discarded", async () => {
  const term = terminal();
  let current = true;
  let reject!: (error: Error) => void;
  const pending = pasteFromClipboard(term.target, () => new Promise<string>((_, fail) => { reject = fail; }), () => current);
  current = false;
  reject(new Error("permission denied"));
  assert.equal(await pending, "stale");
  assert.deepEqual(term.pasted, []);
});

test("an empty clipboard leaves the session untouched and offers a useful retry message", async () => {
  const term = terminal();
  await assert.rejects(pasteFromClipboard(term.target, async () => "", () => true), /没有可粘贴的文字/);
  assert.deepEqual(term.pasted, []);
  assert.equal(term.focused, 0);
});

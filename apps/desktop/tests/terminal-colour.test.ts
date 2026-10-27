/**
 * SGR parsing for tool output: keep the colour, drop the terminal control.
 *
 * The security property is the point of these tests. Output bytes come from a
 * server we do not control, and a terminal is a program: ESC can move the cursor,
 * change the scroll region, query the device, retitle the window or write the
 * clipboard. Only exact `ESC [ <sgr params> m` may survive, and only as a tone.
 */

import assert from "node:assert/strict";
import test from "node:test";

import { hasTerminalEscapes, parseTerminalRuns } from "../src/features/agent/terminal-colour.js";

const ESC = "\u001b";
const text = (runs: ReturnType<typeof parseTerminalRuns>) => runs.map((run) => run.text).join("");
const tones = (runs: ReturnType<typeof parseTerminalRuns>) => runs.map((run) => `${run.tone}:${run.text}`);

test("systemctl-style output keeps its meaning", () => {
  const input = `${ESC}[0;32m● nginx.service - A high performance web server${ESC}[0m\n     Active: ${ESC}[0;32mactive (running)${ESC}[0m\n`;
  assert.deepEqual(tones(parseTerminalRuns(input)), [
    "success:● nginx.service - A high performance web server",
    "plain:\n     Active: ",
    "success:active (running)",
    "plain:\n",
  ]);
});

test("a red FAILED line and a yellow warning are distinguished", () => {
  const input = `${ESC}[31mFAILED${ESC}[0m ${ESC}[33mdegraded${ESC}[0m`;
  assert.deepEqual(tones(parseTerminalRuns(input)), ["error:FAILED", "plain: ", "warning:degraded"]);
});

test("the last colour in one sequence wins, and 0 resets", () => {
  assert.deepEqual(tones(parseTerminalRuns(`${ESC}[32;1mok${ESC}[0m`)), ["bold:ok"]);
  assert.deepEqual(tones(parseTerminalRuns(`${ESC}[1;32mok${ESC}[0m`)), ["success:ok"]);
});

test("extended 256-colour forms are read, not just dropped", () => {
  assert.deepEqual(tones(parseTerminalRuns(`${ESC}[38;5;196mbad${ESC}[0m`)), ["error:bad"]);
  assert.deepEqual(tones(parseTerminalRuns(`${ESC}[38;5;46mgood${ESC}[0m`)), ["success:good"]);
});

test("cursor, scroll-region, title and clipboard escapes never survive", () => {
  const hostile = [
    `${ESC}[2J`, // erase display
    `${ESC}[1;1H`, // cursor home
    `${ESC}[?25l`, // hide cursor (private mode)
    `${ESC}[1;24r`, // set scroll region
    `${ESC}[6n`, // device status report
    `${ESC}c`, // full reset
  ];
  for (const sequence of hostile) {
    const runs = parseTerminalRuns(`before${sequence}after`);
    assert.equal(text(runs), "beforeafter", `leaked control from ${JSON.stringify(sequence)}`);
  }
});

test("an OSC string is consumed whole, so its payload is not shown as text", () => {
  // Window title and clipboard writes are OSC (`ESC ]`), terminated by BEL or ST.
  // If only the introducer were dropped, the payload would appear as log text.
  for (const sequence of [
    `${ESC}]0;pwned\u0007`,
    `${ESC}]52;c;cGF5bG9hZA==\u0007`,
    `${ESC}]2;title${ESC}\\`,
  ]) {
    const runs = parseTerminalRuns(`before${sequence}after`);
    assert.equal(text(runs), "beforeafter", `leaked OSC payload from ${JSON.stringify(sequence)}`);
  }
});

test("no control character ever reaches the rendered text", () => {
  for (const sequence of [`${ESC}[52;c;payload`, `${ESC}[38;5`, `${ESC}(B`, `${ESC}xy`]) {
    const rendered = text(parseTerminalRuns(`a${sequence}b`));
    // eslint-disable-next-line no-control-regex
    assert.equal(/[\u0000-\u0008\u000b-\u001f\u007f]/.test(rendered), false, `control survived ${JSON.stringify(sequence)}`);
  }
});

test("a private mode is not mistaken for a colour", () => {
  // `ESC[?25l` has a `?` parameter; the SGR matcher must reject it outright rather
  // than keeping the text after the `?`.
  assert.deepEqual(parseTerminalRuns(`${ESC}[?25lhidden`), [{ text: "hidden", tone: "plain" }]);
});

test("bare C0 controls are stripped even without an escape", () => {
  // A carriage return would otherwise let a log line overwrite what came before it.
  assert.equal(text(parseTerminalRuns("a\rb\u0007c\u0000d")), "abcd");
});

test("tabs and newlines are preserved because they are layout, not control", () => {
  assert.equal(text(parseTerminalRuns("a\tb\nc")), "a\tb\nc");
});

test("a truncated or malformed sequence is dropped, never half-rendered", () => {
  for (const input of [`x${ESC}[`, `x${ESC}[38`, `x${ESC}[999;999;999m`, `x${ESC}`, `x${ESC}(B`]) {
    const runs = parseTerminalRuns(input);
    assert.equal(text(runs).startsWith("x"), true, `lost the literal text for ${JSON.stringify(input)}`);
    assert.equal(/[\u0000-\u001f]/.test(text(runs).replace(/[\t\n]/g, "")), false);
  }
});

test("plain text is untouched", () => {
  const runs = parseTerminalRuns("no escapes here\n");
  assert.deepEqual(runs, [{ text: "no escapes here\n", tone: "plain" }]);
});

test("detection is cheap and does not false-positive", () => {
  assert.equal(hasTerminalEscapes("plain"), false);
  assert.equal(hasTerminalEscapes(`a${ESC}[0m`), true);
});

test("an extended colour form is consumed as one unit, not read as text", () => {
  // `38;5;196` is one colour. If the parser treated the trailing numbers as
  // ordinary SGR codes they would still be removed here — what must never happen
  // is the digits appearing in the rendered text.
  assert.equal(text(parseTerminalRuns(`${ESC}[38;5;196mx${ESC}[0m`)), "x");
  assert.equal(text(parseTerminalRuns(`${ESC}[38;2;255;0;0mx${ESC}[0m`)), "x");
});

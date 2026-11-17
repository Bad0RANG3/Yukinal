import assert from "node:assert/strict";
import test from "node:test";

import { TerminalOutputFilter, stripTerminalControls } from "../src/lib/terminal-safety.js";

test("terminal drops control sequences and their payload at every chunk boundary", () => {
  for (const control of [
    "\x1b[2J", "\x1b[H", "\x1b[?2004h", "\x1b[6n", "\x1bc", "\x1b(B",
    "\x1b]52;c;clipboard\x07", "\x1b]8;;https://evil.example\x1b\\",
    "\x1bPprivate payload\x07still private\x1b\\", "\x1b_hidden\x1b\\",
    "\x90private\x9c", "\x9d52;c;private\x07", "\x9b2J", "\x1b[999m",
    "\x1b[38;5;256m", "\x1b[38;2;255;0m", "\x1b[48;2;999;0;0m",
  ]) {
    const input = `before${control}after`;
    for (let split = 0; split <= input.length; split += 1) {
      const filter = new TerminalOutputFilter();
      assert.equal(filter.write(input.slice(0, split)) + filter.write(input.slice(split)), "beforeafter",
        `leaked ${JSON.stringify(control)} at ${split}`);
    }
  }
});

test("bounded SGR colours survive unchanged including single-byte chunk delivery", () => {
  const input = "\x1b[0m\x1b[1;32mls\x1b[0m\r\n\x1b[38;5;196mgit diff\x1b[48;2;0;64;255mtruecolor\x1b[m";
  const filter = new TerminalOutputFilter();
  assert.equal([...input].map((character) => filter.write(character)).join(""), input);
});

test("unfinished strings and oversized SGR stay bounded and fail closed", () => {
  const filter = new TerminalOutputFilter();
  assert.equal(filter.write("visible\x1b]52;c;"), "visible");
  assert.equal(filter.write("hidden".repeat(100_000)), "");
  assert.equal(filter.write("\x1b\\after"), "after");
  assert.equal(filter.write(`\x1b[${"1;".repeat(100_000)}32mtext`), "text");
});

test("plain Markdown removes SGR, C0/C1, bidirectional controls and string payloads", () => {
  assert.equal(stripTerminalControls("a\x1b[31mb\x1b[0m\x00\x08\r\x7f\x85\u202ec\t\nd\x1bPsecret\x1b\\"), "abc\t\nd");
});

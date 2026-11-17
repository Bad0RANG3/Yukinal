import assert from "node:assert/strict";
import test from "node:test";
import { terminalTheme } from "../src/features/terminal/terminal-theme.js";

const ansi = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
  "brightBlack", "brightRed", "brightGreen", "brightYellow", "brightBlue", "brightMagenta", "brightCyan", "brightWhite"] as const;

test("both app theme modes provide complete, chromatic ANSI palettes", () => {
  for (const mode of ["light", "dark"] as const) {
    const theme = terminalTheme(mode);
    for (const key of [...ansi, "cursor", "cursorAccent", "selectionBackground", "selectionForeground"] as const) {
      assert.match(theme[key]!, /^#[0-9a-f]{6}$/i, `${mode}.${key}`);
    }
    assert.equal(new Set(ansi.map((key) => theme[key])).size, 16);
    assert.notEqual(theme.red!.slice(1, 3), theme.red!.slice(3, 5), "red must not turn into gray");
    assert.notEqual(theme.background, theme.foreground);
  }
  assert.notDeepEqual(terminalTheme("light"), terminalTheme("dark"));
});

test("app semantic surface and text colours also map to cursor contrast", () => {
  const theme = terminalTheme("dark", "#191919", "#eeeeee");
  assert.equal(theme.background, "#191919");
  assert.equal(theme.cursorAccent, "#191919");
  assert.equal(theme.foreground, "#eeeeee");
  assert.equal(theme.cursor, "#eeeeee");
});

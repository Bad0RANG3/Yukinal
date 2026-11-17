import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const root = fileURLToPath(new URL("../../", import.meta.url));
const desktopRequire = createRequire(join(root, "apps/desktop/package.json"));
const viteRequire = createRequire(desktopRequire.resolve("vite"));
const tinyglobbyRequire = createRequire(viteRequire.resolve("tinyglobby"));
const fdirRequire = createRequire(tinyglobbyRequire.resolve("fdir"));
const patchedPath = join(root, "vendor/picomatch/index.js");
const patched = createRequire(import.meta.url)(patchedPath);
const installedPath = viteRequire.resolve("picomatch");

test("Vite, tinyglobby and fdir all resolve the reviewed picomatch backport", () => {
  const source = readFileSync(join(root, "vendor/picomatch/lib/parse.js"), "utf8");
  for (const requireFrom of [viteRequire, tinyglobbyRequire, fdirRequire]) {
    const resolved = requireFrom.resolve("picomatch");
    assert.equal(readFileSync(join(dirname(resolved), "lib/parse.js"), "utf8"), source);
    const metadata = requireFrom("picomatch/package.json");
    assert.equal(metadata.name, "picomatch");
    assert.equal(metadata.version, "4.0.7", "the original version must not be falsified");
  }
});

test("repeated wildcard extglobs terminate within a child-process deadline", () => {
  // Every attack runs in the same disposable process. A regression can block
  // the RegExp engine, so an in-process timer would not protect the test runner.
  const source = `
    const assert = require('node:assert/strict');
    const pm = require(process.argv[1]);
    const cases = [
      ['+(*)c', 'a'.repeat(100) + 'b'],
      ['+(*a)', 'a'.repeat(100) + '!'],
      ['*(*a)', 'a'.repeat(100) + '!'],
      ['+(a*)b', 'a'.repeat(100) + '!'],
      ['+(a*a)', 'a'.repeat(100) + '!'],
      ['+(**a)', 'a'.repeat(100) + '!'],
      ['+(a|*a)', 'a'.repeat(100) + '!'],
      ['+(@(*a))', 'a'.repeat(100) + '!'],
      ['+([[]*a)', '[a'.repeat(100) + '!'],
      ['+([*a)', '[a'.repeat(100) + '!'],
      ['+([a[]*a)', 'a'.repeat(100) + '!'],
      ['+([[:alpha:]]*a)', 'a'.repeat(100) + '!'],
      ['+([*a])', '[a]'.repeat(60) + '!', { nobracket: true }],
      ['+(a\\\\*)', 'a'.repeat(100) + '!', { unescape: true }]
    ];
    for (const [pattern, input, options] of cases) {
      assert.equal(pm.isMatch(input, pattern, options), false, pattern);
      assert.equal(pm.isMatch(pattern, pattern, options), true, pattern + ' is literalized');
    }
  `;
  const result = spawnSync(process.execPath, ["-e", source, installedPath], {
    cwd: root,
    encoding: "utf8",
    timeout: 5000,
    windowsHide: true,
  });
  assert.equal(result.error, undefined, result.error?.message);
  assert.equal(result.status, 0, result.stderr || result.stdout);
});

test("ordinary build globs and literal-star repeated extglobs keep matching", () => {
  const cases = [
    ["src/components/TerminalPane.tsx", "**/*.{ts,tsx}", true],
    ["src/components/TerminalPane.css", "**/*.{ts,tsx}", false],
    ["apps/desktop/src/main.tsx", "apps/**/src/**/*.tsx", true],
    ["node_modules/vite/index.js", "**/node_modules/**", true],
    ["README.md", "*.md", true],
    ["src/a.ts", "src/?.ts", true],
    ["src/ab.ts", "src/?.ts", false],
    ["file7.txt", "file[0-9].txt", true],
    ["src/.hidden.ts", "src/*.ts", false],
    ["src/a.js", "!**/*.ts", true],
    ["src/a.ts", "!**/*.ts", false],
    ["jsjsx", "+(js|jsx)", true],
    ["foobar", "*(foo|bar)", true],
    ["fffooo", "*(*(f)*(o))", true],
    ["***", "+(\\*)", true],
    ["***", "+([*])", true],
    ["abc", "+([a-z])", true],
    ["[[", "+([[])", true],
  ];
  for (const [input, pattern, expected] of cases) {
    assert.equal(patched.isMatch(input, pattern), expected, `${input} / ${pattern}`);
  }
  assert.equal(patched.isMatch("src/.hidden.ts", "src/*.ts", { dot: true }), true);
  assert.equal(patched.isMatch("src\\a.ts", "src/*.ts", { windows: true }), true);
});

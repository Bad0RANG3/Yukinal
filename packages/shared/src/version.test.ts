/**
 * The release-version gate.
 *
 * Five kinds of file spell the version out (six JavaScript manifests, the Rust
 * workspace, the Tauri bundle config, and the IPC fixtures a packaged UI reads), and
 * before this test existed they could disagree freely: the version was `0.0.0`
 * everywhere, so nothing ever failed. This pins them together, so a release can no
 * longer be half-applied — a bumped `package.json` with a stale `Cargo.toml` is a
 * red test, not a bug report after packaging.
 */

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { APP_VERSION } from "./version.js";

/** This file lives in `packages/shared/src/`, so the repository root is three levels up. */
const rootFile = (relativePath: string): string =>
  fileURLToPath(new URL(`../../../${relativePath}`, import.meta.url));

const readRoot = (relativePath: string): string => readFileSync(rootFile(relativePath), "utf8");

const MANIFESTS = [
  "package.json",
  "apps/desktop/package.json",
  "apps/desktop/src-tauri/tauri.conf.json",
  "apps/agent/package.json",
  "packages/shared/package.json",
  "packages/provider-sdk/package.json",
  "packages/agent-sdk/package.json",
];

test("every JavaScript manifest reports APP_VERSION", () => {
  for (const manifest of MANIFESTS) {
    const parsed = JSON.parse(readRoot(manifest)) as { version?: unknown };
    assert.equal(
      parsed.version,
      APP_VERSION,
      `${manifest} must report the released version (see packages/shared/src/version.ts)`,
    );
  }
});

test("the Rust workspace version matches APP_VERSION", () => {
  const cargo = readRoot("Cargo.toml");
  const start = cargo.indexOf("[workspace.package]");
  assert.notEqual(start, -1, "Cargo.toml must declare a [workspace.package] section");
  // Slice to the next section header so a dependency's `version =` key one table
  // further down can never satisfy this assertion.
  const nextSection = cargo.indexOf("\n[", start + 1);
  const section = cargo.slice(start, nextSection === -1 ? undefined : nextSection);
  const declared = /^\s*version\s*=\s*"([^"]+)"/m.exec(section);
  assert.equal(
    declared?.[1],
    APP_VERSION,
    "Cargo.toml [workspace.package].version is what `core_ping` reports; it must match APP_VERSION",
  );
});

test("the version-carrying IPC fixtures advertise APP_VERSION", () => {
  // `crates/core`'s fixture tests compile these same files in, so a version written
  // here has to be the one the Rust side would serialise.
  const fixtures: Array<[string, (payload: Record<string, unknown>) => unknown]> = [
    ["packages/shared/fixtures/ipc/core_ping.json", (payload) => payload.version],
    ["packages/shared/fixtures/ipc/agent_spawn.json", (payload) => payload.agentVersion],
    ["packages/shared/fixtures/ipc/agent_status.json", (payload) => payload.agentVersion],
    ["packages/shared/fixtures/ipc/agent_status_restarted.json", (payload) => payload.agentVersion],
  ];
  for (const [path, pick] of fixtures) {
    const payload = JSON.parse(readRoot(path)) as Record<string, unknown>;
    assert.equal(pick(payload), APP_VERSION, `${path} must carry the released version`);
  }
});

test("release documentation advertises the current version", () => {
  const readme = readRoot("README.md");
  const changelog = readRoot("docs/changelog.md");
  assert.ok(readme.includes("`" + APP_VERSION + "`"), "README.md must advertise APP_VERSION");
  assert.ok(changelog.includes("## " + APP_VERSION + " "), "docs/changelog.md must contain a dated section for APP_VERSION");
});

test("formal project governance files are present", () => {
  const required = [
    "CONTRIBUTING.md",
    "SECURITY.md",
    "CODE_OF_CONDUCT.md",
    ".github/pull_request_template.md",
    ".github/ISSUE_TEMPLATE/bug_report.yml",
    ".github/ISSUE_TEMPLATE/feature_request.yml",
    ".github/ISSUE_TEMPLATE/config.yml",
  ];
  for (const path of required) {
    assert.doesNotThrow(() => readRoot(path), `${path} must exist for a formal release`);
  }

  const workflow = readRoot(".github/workflows/package.yml");
  assert.match(workflow, /tags:\s*\["v\*"\]/, "the package workflow must run for v* release tags");
});

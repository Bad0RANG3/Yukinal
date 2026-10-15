/**
 * The release-version gate.
 *
 * The JavaScript manifests and the version-carrying IPC fixtures all spell the
 * version out, and before this test existed they could disagree freely: the version
 * was `0.0.0` everywhere, so nothing ever failed. This pins them together, so a
 * release can no longer be half-applied.
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

test("the version-carrying IPC fixtures advertise APP_VERSION", () => {
  // The shared fixtures are the serialised contract the desktop host round-trips in
  // its tests, so a version written here has to be the value the host would emit.
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
});

import assert from "node:assert/strict";
import test from "node:test";

import { buildEvidence } from "./evidence.js";

const target = { host: "remote" as const, serverId: "srv_demo", environment: "staging" as const };

test("evidence envelopes redact secrets and hash the persisted content", () => {
  const evidence = buildEvidence({
    taskId: "task_1",
    target,
    toolName: "filesystem.read",
    input: { path: "/srv/app/.env" },
    output: { content: "API_KEY=fixture-value", path: "/srv/app/.env" },
    collectedAt: "2026-09-19T00:00:00.000Z",
  });

  assert.equal(evidence.kind, "file");
  assert.equal(evidence.redactionStatus, "redacted");
  assert.equal(typeof evidence.content, "object");
  const content = evidence.content as { content: string; path: string };
  assert.match(content.content, /\[redacted\]/);
  assert.equal(content.path, "/srv/app/.env");
  assert.doesNotMatch(content.content, /fixture-value/);
  assert.match(evidence.contentHash, /^[a-f0-9]{64}$/);
});

test("large results become a visible bounded preview", () => {
  const evidence = buildEvidence({
    taskId: "task_1",
    target,
    toolName: "docker.logs",
    input: { container: "api" },
    output: "x".repeat(1_100_000),
    collectedAt: "2026-09-19T00:00:00.000Z",
  });

  assert.equal(evidence.kind, "log");
  assert.equal(evidence.truncated, true);
  assert.equal(typeof evidence.content, "object");
  assert.equal((evidence.content as { truncated: boolean }).truncated, true);
});

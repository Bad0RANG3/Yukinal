import assert from "node:assert/strict";
import test from "node:test";
import { createRef } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type { ApprovalRequest } from "@yukinal/shared";
import { AgentFeed } from "../src/features/agent/AgentFeed.js";
import type { Entry } from "../src/features/agent/transcript.js";

const request = (id: string): ApprovalRequest => ({
  approvalId: id, runId: id, toolName: "server.exec", input: { command: "true" },
  reason: id, factsSummary: [], target: { host: "remote", serverId: "fixture", environment: "unknown" },
  expiresAt: "2099-01-01T00:00:00Z", sessionGrantable: false,
});
const entries: Entry[] = [
  { kind: "user", text: "previous run" }, { kind: "approval", approval: request("old") },
  { kind: "user", text: "current run" }, { kind: "approval", approval: request("current") },
];
const render = (running: boolean, approvalStatuses: Record<string, string> = {}) => renderToStaticMarkup(
  <AgentFeed entries={entries} running={running} runState={null} canSend={false}
    onRetry={() => {}} onApproval={async () => { assert.fail("render must not approve"); }}
    pendingApprovalIds={[]} approvalStatuses={approvalStatuses} feedRef={createRef()} frameRef={createRef()}
    onScroll={() => {}} showJumpControl={false} onJumpToLatest={() => {}} motionScope="fixture" />,
);

test("starting a new run never reactivates an earlier approval card", () => {
  const html = render(true);
  assert.equal((html.match(/>批准一次<\/button>/g) ?? []).length, 1);
  assert.equal((html.match(/本次运行已结束/g) ?? []).length, 1);
  const settled = render(false);
  assert.doesNotMatch(settled, />批准一次<\/button>/);
  assert.equal((settled.match(/本次运行已结束/g) ?? []).length, 2);
});

test("a recorded approval decision is preserved when rendering history", () => {
  const html = render(true, { old: "已批准一次", current: "审批已过期" });
  assert.match(html, /已批准一次/);
  assert.match(html, /审批已过期/);
  assert.doesNotMatch(html, />批准一次<\/button>|本次运行已结束/);
});

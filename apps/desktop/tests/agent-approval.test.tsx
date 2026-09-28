/**
 * 审批卡片上的「本次运行批准」按钮。
 *
 * ADR 0072 让会话授权对一部分 high 动作生效，但也仅限于此。旧界面无论引擎会
 * 不会记住授权，都渲染这个按钮；点了之后引擎只是静默忽略，用户以为自己批准了
 * 一整轮，实际下一次还会再问。这条测试把「按钮只在真的会记住时出现」钉住。
 */

import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";

import type { ApprovalRequest } from "@yukinal/shared";

import { AgentEntryView } from "../src/features/agent/AgentEntryView.js";
import type { Entry } from "../src/features/agent/transcript.js";

const noop = async (): Promise<void> => {};

const approval = (overrides: Partial<ApprovalRequest> = {}): ApprovalRequest => ({
  approvalId: "apr_1",
  runId: "run_1",
  toolName: "docker.restart",
  input: {},
  reason: "restart the container",
  factsSummary: [],
  target: { host: "remote", serverId: "srv_1", environment: "staging" },
  expiresAt: "2026-01-01T00:01:00Z",
  ...overrides,
});

function render(entry: Entry): string {
  return renderToStaticMarkup(
    <AgentEntryView entry={entry} onApproval={noop} approvalBusy={false} />,
  );
}

test("a non-session-grantable approval drops the button and explains why", () => {
  const markup = render({ kind: "approval", approval: approval({ sessionGrantable: false }) });
  assert.equal(markup.includes("本次运行批准"), false, "the button would lie about what the engine remembers");
  assert.ok(markup.includes("此操作每次都需要单独批准"), "the card must say why there is no button");
  // The per-call approval is still offered.
  assert.ok(markup.includes("批准一次"));
  assert.ok(markup.includes("拒绝"));
});

test("an absent or true sessionGrantable keeps the remember button", () => {
  const cases: Array<[string, Partial<ApprovalRequest>]> = [
    ["absent (older sidecar)", {}],
    ["true", { sessionGrantable: true }],
  ];
  for (const [label, overrides] of cases) {
    const markup = render({ kind: "approval", approval: approval(overrides) });
    assert.ok(markup.includes("本次运行批准"), `sessionGrantable ${label} must keep the button`);
    assert.equal(markup.includes("此操作每次都需要单独批准"), false);
  }
});

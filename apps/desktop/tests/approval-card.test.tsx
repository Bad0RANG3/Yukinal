import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import type { ApprovalRequest } from "@yukinal/shared";
import { ApprovalCard } from "../src/features/agent/ApprovalCard.js";

const request: ApprovalRequest = {
  approvalId: "approval", runId: "run", toolName: "server.exec",
  input: { command: "set -e\nprintf 'hello'\n", purpose: "检查部署状态", timeoutMs: 20000 },
  reason: "server.exec needs your approval before it can run.",
  factsSummary: ["Risk level: destructive", "The tool has not run yet.", "Custom policy: preserve /srv/data"],
  target: { host: "remote", serverId: "srv_example", environment: "production" },
  expiresAt: "2099-01-01T00:00:00Z", sessionGrantable: false,
};
const render = (approval = request, busy = false, status?: string) => renderToStaticMarkup(
  <ApprovalCard approval={approval} busy={busy} status={status} onApproval={async () => assert.fail("render cannot approve")} />,
);

test("command review preserves full multiline content and every other argument", () => {
  const command = "set -e\n" + "# long command\n".repeat(500) + "echo END_OF_REVIEW";
  const html = render({ ...request, input: { command, purpose: "检查部署状态", timeoutMs: 20000, extra: "important" } });
  assert.ok(html.includes(command));
  assert.match(html, /全部参数/);
  assert.match(html, /&quot;timeoutMs&quot;: 20000/);
  assert.match(html, /&quot;extra&quot;: &quot;important&quot;/);
  assert.match(html, /approval-input" open=""/);
  assert.match(html, /风险：破坏性操作/);
  assert.match(html, /尚未执行/);
  assert.match(html, /Custom policy: preserve \/srv\/data/);
  assert.match(html, /srv_example · 生产环境/);
  assert.doesNotMatch(html, /needs your approval|本次运行批准/);
});

test("settled approvals collapse review and busy approvals cannot be submitted again", () => {
  const settled = render(request, false, "已批准一次");
  assert.match(settled, /role="status">已批准一次/);
  assert.doesNotMatch(settled, /approval-input" open=|>批准一次<\/button>|>拒绝<\/button>/);
  const busy = render(request, true);
  assert.match(busy, /aria-busy="true"/);
  assert.match(busy, /正在提交…/);
  assert.equal((busy.match(/class="approval-button [^"]+" disabled=""/g) ?? []).length, 2);
});

test("non-command calls keep complete input, unknown reasons and safe escaping", () => {
  const html = render({ ...request, toolName: "file.write", reason: "Protected path requires review", input: { path: "/srv/config", content: "<script>unsafe()</script>" }, sessionGrantable: true });
  assert.match(html, /Protected path requires review/);
  assert.match(html, /操作参数/);
  assert.match(html, /&lt;script&gt;unsafe\(\)&lt;\/script&gt;/);
  assert.doesNotMatch(html, /<script>/);
  assert.match(html, /本次运行批准/);
});

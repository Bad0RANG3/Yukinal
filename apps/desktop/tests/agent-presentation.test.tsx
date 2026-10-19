import assert from "node:assert/strict";
import test from "node:test";
import { Children, isValidElement, type ComponentProps, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type { AgentContextUsage, AgentTokenUsage, ApprovalDecision, ApprovalRequest } from "@yukinal/shared";

import { CopyButton } from "../src/components/CopyButton.js";
import { ApprovalCard } from "../src/features/agent/ApprovalCard.js";
import { AgentEntryView } from "../src/features/agent/AgentEntryView.js";
import { AgentRunStatus } from "../src/features/agent/AgentHeader.js";
import { NativeTaskApprovalCards } from "../src/features/agent/NativeTaskApprovals.js";
import type { NativeTaskSnapshot } from "../src/features/agent/native-task-runs.js";

const noop = async () => {};
const ticket: ApprovalRequest = {
  approvalId: "apr_fixture", runId: "run_fixture", callId: "call_fixture", toolName: "server.exec",
  input: Object.freeze({ command: "printf '<review>'", maxOutputBytes: 4096, timeoutMs: 10000 }),
  reason: "核对当前身份", factsSummary: ["仅执行计划中已绑定的一步"],
  target: { host: "remote", serverId: "srv_fixture", environment: "staging" },
  sessionGrantable: false, expiresAt: "2026-10-19T00:00:00Z",
};
const context: AgentContextUsage = {
  totalTurns: 18, includedTurns: 8, compactedTurns: 10, inputBytes: 4096, maxInputBytes: 65536,
  memorySources: [{ id: "memory_fixture", scope: "global", title: "用户提供的约定", updatedAt: "2026-10-18T00:00:00Z" }],
};
function status(contextUsage?: AgentContextUsage | null, tokenUsage: AgentTokenUsage | null = null): string {
  return renderToStaticMarkup(<AgentRunStatus running={false} runState={null} agentReady shell contextUsage={contextUsage} tokenUsage={tokenUsage} />);
}
function elements(node: ReactNode): ReactElement<Record<string, unknown>>[] {
  return Children.toArray(node).flatMap((child) => isValidElement<Record<string, unknown>>(child)
    ? [child, ...elements(child.props.children as ReactNode)] : []);
}
// The memo wrapper exposes the real pure renderer; no DOM or replacement approval runtime is used.
const renderEntryTree = (AgentEntryView as unknown as { type: (props: ComponentProps<typeof AgentEntryView>) => ReactNode }).type;

test("ordinary and native approvals share the complete real input review before the same decisions", () => {
  const shared = renderToStaticMarkup(<ApprovalCard approval={ticket} onApproval={noop} busy={false} />);
  const ordinary = renderToStaticMarkup(<AgentEntryView entry={{ kind: "approval", approval: ticket }} onApproval={noop} approvalBusy={false} />);
  const snapshot: NativeTaskSnapshot = { error: null, runs: [{
    runId: ticket.runId, taskId: "task_fixture", sessionId: "session_fixture", serverId: "srv_fixture",
    title: "有限任务", state: "waiting_approval", approval: ticket, pending: false,
  }] };
  const native = renderToStaticMarkup(<NativeTaskApprovalCards snapshot={snapshot} scope={{ taskId: "task_fixture" }} />);
  assert.equal(ordinary, shared);
  assert.ok(native.includes(shared));
  assert.equal((native.match(/approval-input-json/g) ?? []).length, 1);
  const exactInput = renderToStaticMarkup(<pre className="agent-entry-body approval-input-json">{JSON.stringify(ticket.input, null, 2)}</pre>);
  assert.ok(shared.includes(exactInput));
  assert.ok(shared.includes(ticket.factsSummary[0]!));
  assert.ok(shared.indexOf("approval-input-json") < shared.indexOf("approval-actions"));
  assert.doesNotMatch(shared, /本次运行批准|已批准/);
  assert.deepEqual(ticket.input, { command: "printf '<review>'", maxOutputBytes: 4096, timeoutMs: 10000 });
});

test("approval clicks forward the original ticket and only the existing decisions", () => {
  const calls: Array<[ApprovalRequest, ApprovalDecision]> = [];
  const actual = { ...ticket, sessionGrantable: true };
  const tree = ApprovalCard({ approval: actual, busy: false, onApproval: async (approval, decision) => { calls.push([approval, decision]); } });
  const buttons = elements(tree).filter((element) => element.type === "button");
  for (const button of buttons) (button.props.onClick as () => void)();
  assert.deepEqual(calls.map(([, decision]) => decision), ["reject", "approve_once", "approve_session"]);
  assert.ok(calls.every(([approval]) => approval === actual));
});

test("busy and resolved approvals remain driven by the shared caller state", () => {
  const pending = renderToStaticMarkup(<ApprovalCard approval={ticket} onApproval={noop} busy />);
  assert.match(pending, /aria-busy="true"/);
  assert.equal((pending.match(/disabled=""/g) ?? []).length, 2);
  assert.match(pending, /正在提交审批决定/);
  assert.doesNotMatch(pending, /已批准/);
  const resolved = renderToStaticMarkup(<ApprovalCard approval={ticket} onApproval={noop} busy={false} status="宿主确认已拒绝" />);
  assert.match(resolved, /宿主确认已拒绝/);
  assert.doesNotMatch(resolved, /approval-actions/);
  assert.match(resolved, /approval-input-json/);
});

test("reasoning stays user-collapsible and its summary uses only the actual text count", () => {
  const markup = renderToStaticMarkup(<AgentEntryView entry={{ kind: "reasoning", text: "检查🙂" }} onApproval={noop} approvalBusy={false} />);
  assert.match(markup, /<details class="agent-reasoning">/);
  assert.match(markup, /<summary class="agent-reasoning-summary">/);
  assert.match(markup, /agent-reasoning-indicator/);
  assert.match(markup, /3 字符/);
  assert.match(markup, /检查🙂/);
  assert.doesNotMatch(markup, /\bopen=|思考中|耗时|\d+秒/);
});

test("assistant copy receives the untrimmed complete answer and keeps real participant metadata", () => {
  const answer = "  # 回答\n\n```sh\nprintf '<exact>'\n```\n";
  const entry: ComponentProps<typeof AgentEntryView>["entry"] = {
    kind: "assistant", text: answer, model: "deepseek-chat",
    participant: { id: "worker_fixture", label: "验证助手", kind: "subagent" },
  };
  const tree = renderEntryTree({ entry, onApproval: noop, approvalBusy: false });
  const copy = elements(tree).find((element) => element.type === CopyButton);
  assert.ok(copy);
  assert.equal(copy.props.text, answer);
  assert.equal(copy.props.ariaLabel, "复制完整回答");
  const markup = renderToStaticMarkup(<AgentEntryView entry={entry} onApproval={noop} approvalBusy={false} />);
  assert.match(markup, /验证助手/); assert.match(markup, /deepseek-chat/); assert.match(markup, /子 Agent/);
  const empty = renderEntryTree({ entry: { kind: "assistant", text: "" }, onApproval: noop, approvalBusy: false });
  assert.equal(elements(empty).some((element) => element.type === CopyButton), false);
});

test("context disclosure separates history, text budget and partial model-reported tokens", () => {
  const safeContext = { ...context, systemPrompt: "PRIVATE_PROMPT_SENTINEL", memorySources: [{ ...context.memorySources![0]!, content: "PRIVATE_MEMORY_SENTINEL" }] };
  const markup = status(safeContext, { inputTokens: 123, outputTokens: 9, reportedRequests: 2, unreportedRequests: 1 });
  assert.match(markup, /<details class="agent-context-details">/);
  assert.match(markup, /<summary[^>]+aria-label="查看上下文与用量"/);
  assert.match(markup, /tabindex="0" role="region" aria-label="上下文与用量详情"/);
  assert.match(markup, /保留 8 \/ 18 条/); assert.match(markup, /已压缩 10 条/);
  assert.match(markup, /4\.0 \/ 64 KiB/);
  assert.match(markup, /输入 ≥ 123 tokens；输出 ≥ 9 tokens/);
  assert.match(markup, /1 次模型请求未上报用量/);
  assert.match(markup, /用户提供的约定/);
  assert.doesNotMatch(markup, /PRIVATE_|%|role="progressbar"|\bopen=/);
  const tree = AgentRunStatus({ running: false, runState: null, tokenUsage: null, contextUsage: context, agentReady: true, shell: true });
  const disclosure = elements(tree).find((element) => element.type === "details");
  assert.ok(disclosure);
  let focused = 0;
  let stopped = 0;
  const target = { open: true, querySelector: (selector: string) => { assert.equal(selector, "summary"); return { focus: () => { focused += 1; } }; } };
  const onKeyDown = disclosure.props.onKeyDown as (event: { key: string; currentTarget: typeof target; stopPropagation: () => void }) => void;
  onKeyDown({ key: "Enter", currentTarget: target, stopPropagation: () => { stopped += 1; } });
  assert.equal(target.open, true);
  onKeyDown({ key: "Escape", currentTarget: target, stopPropagation: () => { stopped += 1; } });
  assert.equal(target.open, false);
  assert.equal(focused, 1); assert.equal(stopped, 1);
});

test("missing model usage never becomes zero tokens or a model-capacity percentage", () => {
  const missing = status(null, { inputTokens: 0, outputTokens: 0, reportedRequests: 0, unreportedRequests: 3 });
  assert.match(missing, /用量未上报/); assert.match(missing, /模型用量尚未上报/);
  assert.match(missing, /3 次模型请求未上报用量/);
  assert.doesNotMatch(missing, /0 tokens|%/);
  assert.match(status(context), /模型用量尚未上报/);
  assert.doesNotMatch(status(), /agent-context-details|KiB|tokens|%/);
  const legacy = status(null, { inputTokens: 20, outputTokens: 4 });
  assert.match(legacy, /24 tokens/);
  assert.match(legacy, /输入 20 tokens；输出 4 tokens/);
  assert.doesNotMatch(legacy, /已上报请求|尚未上报/);
});

test("only explicitly new non-approval rows receive the entrance class", () => {
  for (const entry of [{ kind: "assistant", text: "新回答" }, { kind: "reasoning", text: "实际思考" }, { kind: "system", text: "提示" }, { kind: "error", text: "错误" }, { kind: "user", text: "你好" }] as const) {
    const old = renderToStaticMarkup(<AgentEntryView entry={entry} onApproval={noop} approvalBusy={false} />);
    const fresh = renderToStaticMarkup(<AgentEntryView entry={entry} onApproval={noop} approvalBusy={false} isNewEntry />);
    assert.doesNotMatch(old, /is-new-entry/);
    assert.match(fresh, /is-new-entry/);
  }
  const approval = renderToStaticMarkup(<AgentEntryView entry={{ kind: "approval", approval: ticket }} onApproval={noop} approvalBusy={false} isNewEntry />);
  assert.doesNotMatch(approval, /is-new-entry/);
});

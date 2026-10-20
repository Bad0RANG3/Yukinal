import assert from "node:assert/strict";
import test from "node:test";
import { createRef } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { AgentEntryView, toolActionLabel } from "../src/features/agent/AgentEntryView.js";
import { AgentFeed, intermediateReplyIndices, toolActivityRows } from "../src/features/agent/AgentFeed.js";
import { ConversationScrollRail } from "../src/features/agent/ConversationScrollRail.js";
import { FeedFollowIntent } from "../src/features/agent/feed-follow.js";
import { agentFailureLabel, toolPurpose, toolOutcomeFromEvent, type Entry } from "../src/features/agent/transcript.js";

const tool: Extract<Entry, { kind: "tool" }> = { kind: "tool", toolName: "server.info", callId: "call1", target: "staging", riskLevel: "read", decision: "auto", result: { status: "success", durationMs: 123, summary: "safe summary" } };
const renderTool = (entry = tool) => renderToStaticMarkup(<AgentEntryView entry={entry} onApproval={async () => { assert.fail("must not approve"); }} approvalBusy={false} />);

test("progress follows real turn activity and never relabels a previous final reply", () => {
  const entries: Entry[] = [
    { kind: "user", text: "first" }, { kind: "assistant", text: "first final" },
    { kind: "user", text: "deploy" }, { kind: "assistant", text: "image ready; checking login" },
    tool, { kind: "assistant", text: "verified final" },
  ];
  assert.deepEqual([...intermediateReplyIndices(entries, false, "completed")], [3]);
  assert.deepEqual([...intermediateReplyIndices(entries, true, "thinking")], [5, 3]);
  assert.deepEqual([...intermediateReplyIndices(entries, false, "failed")], [5, 3]);
  assert.deepEqual([...intermediateReplyIndices(entries, false, "cancelled")], [5, 3]);
  const html = renderToStaticMarkup(<AgentEntryView entry={entries[3]!} intermediate onApproval={async () => {}} approvalBusy={false} />);
  assert.match(html, /data-reply-phase="progress"/); assert.match(html, /进展/);
  assert.match(html, /checking login/); assert.match(html, /复制完整进展/);
  assert.doesNotMatch(html, /思考过程|任务完成/);
});

test("long tool activity folds completed work without hiding live work, errors or turn anchors", () => {
  const success = Array.from({ length: 30 }, (_, index) => ({ ...tool, entryId: `tool-${index}`, callId: String(index), purpose: `检查 ${index}` }));
  const failed: Entry = { ...tool, callId: "failed", result: { status: "failed", durationMs: 1, summary: "read failed" } };
  const pending: Entry = { ...tool, callId: "pending", result: undefined };
  const entries: Entry[] = [{ kind: "user", text: "完整部署" }, ...success, failed, pending, { kind: "assistant", text: "继续验收" }];
  const rows = toolActivityRows(entries);
  assert.deepEqual(rows.map((row) => row.length), [1, 30, 1, 1, 1]);
  assert.equal(rows[2]![0]!.entry, failed); assert.equal(rows[3]![0]!.entry, pending);
  const html = renderToStaticMarkup(<AgentFeed entries={entries} running={true} runState="running" canSend={false} onRetry={() => {}} onApproval={async () => {}} pendingApprovalIds={[]} approvalStatuses={{}} feedRef={createRef<HTMLDivElement>()} frameRef={createRef<HTMLDivElement>()} onScroll={() => {}} showJumpControl={false} onJumpToLatest={() => {}} motionScope="test" />);
  assert.match(html, /执行了 30 项操作/);
  assert.match(html, /<details class="agent-tool-activity"><summary>/);
  assert.equal((html.match(/data-conversation-turn=""/g) ?? []).length, 1);
  assert.match(html, /read failed/); assert.match(html, /继续验收/);
  assert.equal((html.match(/tool-card-disclosure/g) ?? []).length, 32);
});

test("tool feedback is a single collapsed result row with technical facts inside its disclosure", () => {
  const html = renderTool();
  assert.match(html, /读取服务器信息/); assert.match(html, /tool-card-disclosure/); assert.match(html, /123 ms/); assert.match(html, /safe summary/);
  assert.doesNotMatch(html, /<details[^>]*\sopen[=> ]/);
  const heading = html.split("</summary>")[0]!;
  assert.doesNotMatch(heading, /123 ms|风险：|自动放行|safe summary/);
  assert.equal(toolActionLabel("ssh.disk_usage"), "读取磁盘用量"); assert.equal(toolActionLabel("custom.read"), "custom.read");
});

test("failed tools prioritize already-redacted error text and escape it as text", () => {
  const outcome = toolOutcomeFromEvent({ toolName: "package.inspect", status: "failed", durationMs: 2, outputSummary: "execution_failed", errorCode: "execution_failed", error: 'package inspect returned unexpected extra fields <script>alert(1)</script>' });
  assert.equal(outcome.errorCode, "execution_failed"); assert.match(outcome.error!, /unexpected extra fields/);
  const html = renderTool({ ...tool, toolName: "package.inspect", result: outcome });
  assert.match(html, /tool-failure-reason/); assert.match(html, /package inspect returned unexpected extra fields/); assert.match(html, /&lt;script&gt;/); assert.doesNotMatch(html, /<script/);
  assert.match(renderTool({ ...tool, result: { status: "failed", durationMs: 1, summary: "fallback summary" } }), /fallback summary/);
  assert.equal(toolOutcomeFromEvent({ toolName: "filesystem.read", status: "success", durationMs: 1, outputSummary: "secret file body", error: "unused error" }).error, undefined);
});

test("tool purposes clarify the action while real tool identity and failure limits remain visible", () => {
  const purpose = toolPurpose({ purpose: "检查缓存 <script>" });
  const html = renderTool({ ...tool, purpose });
  assert.match(html, /检查缓存 &lt;script&gt;/);
  assert.match(html, /title="server.info"/);
  assert.match(html, /tool-call-name">server.info/);
  assert.equal(toolPurpose({ purpose: 12 }), undefined);
  assert.equal(toolPurpose({ purpose: " " }), undefined);
  assert.match(agentFailureLabel("tool_call_limit_exceeded"), /达到工具调用次数上限/);
  assert.match(agentFailureLabel("tool_repetition_detected"), /已停止重复调用/);
  assert.equal(agentFailureLabel("unrecognized error"), "unrecognized error");
});

test("command results display readable lines and preserve error output without rendering markup", () => {
  const summary = JSON.stringify({ stdout: "first line\n<script>literal</script>", stderr: "diagnostic\nsecond line", exitCode: 2, stdoutTruncated: true });
  const html = renderTool({ ...tool, toolName: "server.exec", result: { status: "failed", durationMs: 1, error: "Command failed", summary } });
  assert.match(html, /退出状态 2/);
  assert.match(html, /first line\n&lt;script&gt;literal&lt;\/script&gt;/);
  assert.match(html, /diagnostic\nsecond line/);
  assert.match(html, /输出已截断/);
  assert.match(html, /原始摘要/);
  assert.doesNotMatch(html, /<script/);
  assert.match(renderTool({ ...tool, toolName: "server.exec", result: { status: "success", durationMs: 1, summary: "incomplete {" } }), /incomplete \{/);
});

test("received command summaries survive ingestion and render their tail after expansion", () => {
  const outputSummary = JSON.stringify({ stdout: "line\n".repeat(200) + "VERIFIED-TAIL", stderr: "", exitCode: 0 });
  const result = toolOutcomeFromEvent({ toolName: "server.exec", status: "success", durationMs: 1, outputSummary });
  assert.equal(result.summary, outputSummary);
  const html = renderTool({ ...tool, toolName: "server.exec", result });
  assert.match(html, /退出状态 0/);
  assert.match(html, /VERIFIED-TAIL<\/pre>/);
  assert.doesNotMatch(html, /<details[^>]*\sopen[=> ]/);
});

test("turn navigation renders exactly one actionable marker per user turn and no generated tick texture", () => {
  const html = renderToStaticMarkup(<ConversationScrollRail viewportRef={createRef<HTMLDivElement>()} viewportId="feed" turns={[{ id: "a", entryIndex: 0, preview: "第一问" }, { id: "b", entryIndex: 7, preview: "第二问" }]} />);
  assert.equal((html.match(/class="conversation-turn-mark"/g) ?? []).length, 2); assert.match(html, /aria-label="对话轮次"/); assert.match(html, /aria-current="step"/); assert.match(html, /第 2 轮：第二问/); assert.doesNotMatch(html, /role="scrollbar"|conversation-scroll-ticks/);
  assert.equal(renderToStaticMarkup(<ConversationScrollRail viewportRef={createRef<HTMLDivElement>()} viewportId="feed" turns={[]} />), "");
});

test("explicit turn navigation detaches following before keyboard or pointer jumps and stays detached on stream growth", () => {
  const follow = new FeedFollowIntent();
  const at = (top: number, content = 3000) => ({ scrollTop: top, scrollHeight: content, clientHeight: 500 });
  follow.measure(at(2500)); follow.navigateToTurn(at(2500)); follow.scrolled(at(1000));
  assert.deepEqual(follow.measure(at(1000, 5000)), { target: null, showJumpControl: true });
  follow.pinToLatest(); assert.equal(follow.measure(at(1000, 5000)).target, 4500);
});

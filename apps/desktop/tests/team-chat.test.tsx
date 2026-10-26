import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import type { ComponentProps } from "react";
import type { AgentPromptPart, ChatMessage } from "@yukinal/shared";

import { AgentEntryView } from "../src/features/agent/AgentEntryView.js";
import { AgentHeader, AgentRunStatus } from "../src/features/agent/AgentHeader.js";
import { AgentComposer } from "../src/features/agent/AgentComposer.js";
import { appendAssistantDelta, entriesFromMessages, settleAssistantText } from "../src/features/agent/transcript.js";
import { conversationKey, recalledDraft, rememberDraft, type SessionDraft } from "../src/features/agent/session-drafts.js";
import { matchMentions } from "../src/features/agent/composer-triggers.js";

test("actor and model survive streaming, terminal reconciliation and history restoration", () => {
  const identity = { participant: { id: "blue-logs", label: "蓝队 · 日志分析", kind: "agent" as const }, model: "actual-model" };
  const streamed = appendAssistantDelta([{ kind: "user", text: "排查日志" }], "证据", identity);
  const terminal = settleAssistantText(streamed, "证据已整理", identity);
  assert.equal(terminal.length, 2);
  const row = terminal[1]!;
  assert.equal(row.kind, "assistant");
  if (row.kind !== "assistant") return;
  assert.deepEqual(row.participant, identity.participant);
  assert.equal(row.model, identity.model);
  const messages: ChatMessage[] = [{ id: "msg-1", sessionId: "ses-1", role: "assistant", content: row.text, createdAt: "", ...identity }];
  assert.equal(row.entryId, streamed[1]?.entryId, "terminal calibration retains the live row id");
  assert.deepEqual(entriesFromMessages(messages), [{ ...row, entryId: messages[0]!.id }]);
  assert.deepEqual(entriesFromMessages(messages), entriesFromMessages(messages.map((message) => ({ ...message }))));
  const markup = renderToStaticMarkup(<AgentEntryView entry={row} onApproval={async () => {}} approvalBusy={false} />);
  assert.ok(markup.includes(identity.participant.label));
  assert.ok(markup.includes(identity.model));
});

test("different participants remain separate replies in the shared conversation", () => {
  const first = appendAssistantDelta([], "防守证据", { participant: { id: "blue", label: "蓝队", kind: "agent" } });
  const second = appendAssistantDelta(first, "实现建议", { participant: { id: "yellow", label: "黄队", kind: "agent" } });
  assert.equal(second.length, 2);
  assert.equal(second[0]?.kind === "assistant" ? second[0].text : undefined, "防守证据");
  assert.equal(second[1]?.kind === "assistant" ? second[1].text : undefined, "实现建议");
});

test("switching tasks or servers retains independent drafts and attachments", () => {
  const drafts = new Map<string, SessionDraft>();
  const attachments: AgentPromptPart[] = [{ type: "text", text: "context" }];
  rememberDraft(drafts, "server-a", "session-1", { prompt: "未完成的任务 A", attachments });
  rememberDraft(drafts, "server-a", "session-2", { prompt: "任务 B", attachments: [] });
  rememberDraft(drafts, "server-b", "session-1", { prompt: "另一台服务器", attachments: [] });
  rememberDraft(drafts, null, null, { prompt: "全局草稿", attachments: [] });
  attachments.length = 0;
  const restored = recalledDraft(drafts, "server-a", "session-1");
  assert.equal(restored.prompt, "未完成的任务 A");
  assert.equal(restored.attachments.length, 1);
  restored.attachments.length = 0;
  assert.equal(recalledDraft(drafts, "server-a", "session-1").attachments.length, 1);
  assert.equal(recalledDraft(drafts, "server-a", "session-2").prompt, "任务 B");
  assert.equal(recalledDraft(drafts, "server-b", "session-1").prompt, "另一台服务器");
  assert.equal(recalledDraft(drafts, null, null).prompt, "全局草稿");
  assert.deepEqual(recalledDraft(drafts, "server-a", null), { prompt: "", attachments: [] });
  assert.notEqual(conversationKey(null, "null"), conversationKey("null", null));
});

test("the task switcher reports context only from backend turn counts", () => {
  const header = (contextUsage?: { totalTurns: number; includedTurns: number; compactedTurns: number; inputBytes: number; maxInputBytes: number }) => renderToStaticMarkup(<AgentHeader
    running={false} runState={null} tokenUsage={null} agentReady agentOpen shell historyOpen={false}
    historyButtonRef={{ current: null }} closeButtonRef={{ current: null }} onToggleHistory={() => {}} onToggle={() => {}}
    workspace taskTitle="当前任务" contextUsage={contextUsage}
  />);
  assert.equal(header().includes("agent-context-usage"), false);
  const markup = header({ totalTurns: 18, includedTurns: 8, compactedTurns: 10, inputBytes: 4096, maxInputBytes: 65536 });
  assert.ok(markup.replace(/<[^>]+>/g, "").includes("上下文 8/18"));
  assert.ok(markup.includes("已压缩 10 条"));
  assert.ok(markup.includes('aria-label="切换任务：当前任务"'));
  assert.equal(markup.includes("%"), false);
});

test("a running task keeps the switcher focusable and explains why it is inert", () => {
  const markup = renderToStaticMarkup(<AgentHeader
    running runState="thinking" tokenUsage={null} agentReady agentOpen shell historyOpen={false}
    historyButtonRef={{ current: null }} closeButtonRef={{ current: null }} onToggleHistory={() => {}} onToggle={() => {}}
    workspace taskTitle="当前任务"
  />);
  assert.equal(markup.includes('disabled=""'), false, "a disabled button would drop focus mid-run");
  assert.match(markup, /class="agent-task-trigger [^"]*"[^>]*aria-disabled="true"/u);
  assert.ok(markup.includes('title="运行中，结束后可切换任务"'));
});

test("compact header controls retain explicit names and keyboard buttons", () => {
  // The workpage launcher moved out of this row into `WorkspaceTabStrip`; what is
  // pinned here is that whatever a workspace passes in keeps its own accessible
  // name and that nothing in the row lands in the untabbable state.
  const markup = renderToStaticMarkup(<AgentHeader
    running={false} runState={null} tokenUsage={null} agentReady agentOpen shell historyOpen={false}
    historyButtonRef={{ current: null }} closeButtonRef={{ current: null }} onToggleHistory={() => {}} onToggle={() => {}}
    workspace taskTitle="当前任务" workspaceControls={<>
      <button type="button" aria-label="浏览器预览" className="live-preview-control"><span className="live-preview-label">浏览器预览</span></button>
      <button type="button" aria-label="连接设置" className="workspace-guide-toggle"><span>连接设置</span></button>
    </>}
  />);
  assert.ok(markup.includes('class="agent-workspace-controls"'));
  assert.ok(markup.includes('aria-label="浏览器预览"'));
  assert.ok(markup.includes('aria-label="连接设置"'));
  assert.equal(markup.includes('tabindex="-1"'), false);
});

test("English team aliases find the same visible mention without changing the inserted label", () => {
  const candidates = [{ id: "team:red", label: "红队", aliases: ["red", "red-team"], detail: "授权攻击测试" }];
  assert.equal(matchMentions(candidates, "red")[0]?.label, "红队");
  assert.equal(matchMentions(candidates, "红")[0]?.id, "team:red");
  assert.equal(matchMentions(candidates, "blue").length, 0);
});

const composerProps: ComponentProps<typeof AgentComposer> = {
  prompt: "检查连接", attachments: [], onPromptChange: () => {}, onAttachmentsChange: () => {},
  onSubmit: () => {}, onStop: () => {}, running: false, stopping: false, canSend: true, canStop: false,
  permissionMode: "ask", onPermissionModeChange: () => {}, runMode: "goal", onRunModeChange: () => {},
  delivery: "async", onDeliveryChange: () => {}, runPolicy: null, onRunPolicyChange: () => {}, targetEnvironment: "unknown",
  models: [{ key: "provider:model", label: "vendor/model" }], selectedModelKey: "provider:model", selectedModelLabel: "Provider · model", onSelectModel: () => {},
  mentions: [],
  commandContext: { running: false, archived: false, desktop: true },
};

test("the message box is a real combobox, so a screen reader follows the suggestion list", () => {
  // `aria-autocomplete` / `aria-controls` / `aria-activedescendant` are only meaningful
  // on a combobox. Without `role="combobox"` and `aria-expanded`, a screen reader does
  // not announce that the textarea owns a popup list, and the highlighted suggestion is
  // never spoken — keyboard navigation works but is invisible to assistive technology.
  const markup = renderToStaticMarkup(<AgentComposer {...composerProps} />);
  const textarea = markup.match(/<textarea[^>]*>/u)?.[0] ?? "";
  assert.ok(textarea, "the composer must render its textarea");
  assert.match(textarea, /role="combobox"/);
  assert.match(textarea, /aria-expanded="false"/, "a closed composer must say the list is closed");
  assert.match(textarea, /aria-haspopup="listbox"/);
  assert.match(textarea, /aria-autocomplete="list"/);
  // Closed, it must not point at an id that is not in the document.
  assert.doesNotMatch(textarea, /aria-controls=|aria-activedescendant=/);
});

test("running composer preserves stop admission, allows drafting and locks model controls", () => {
  const markup = renderToStaticMarkup(<AgentComposer {...composerProps} running canStop stopping={false} />);
  assert.doesNotMatch(markup, /<textarea[^>]+disabled=""/u);
  assert.match(markup, /<button[^>]+aria-label="模型与推理强度：[^"]+"[^>]+disabled=""/u);
  assert.equal(markup.includes('aria-label="发送消息"'), false);
  const stop = markup.match(/<button[^>]+aria-label="停止运行"[^>]*>/u)?.[0];
  assert.ok(stop);
  assert.equal(stop.includes('disabled=""'), false);
  const stopping = renderToStaticMarkup(<AgentComposer {...composerProps} running stopping canStop={false} />);
  assert.match(stopping, /<button[^>]+aria-label="停止运行"[^>]+disabled=""/u);
  assert.ok(stopping.includes("停止中"));
});

test("composer separates attachments and settings while retaining a precise send blocker", () => {
  const markup = renderToStaticMarkup(<AgentComposer {...composerProps} canSend={false} sendDisabledReason="请先启动运行服务" />);
  assert.equal((markup.match(/class="agent-composer-file-input"/gu) ?? []).length, 1);
  assert.equal((markup.match(/class="agent-composer-toolbar-left"/gu) ?? []).length, 1);
  assert.ok(markup.includes('aria-label="添加附件"'));
  assert.ok(markup.includes('aria-label="对话设置：目标模式，每次询问"'));
  assert.ok(markup.includes('class="agent-composer-action"'));
  assert.equal(markup.includes('aria-label="添加图片"'), false);
  assert.equal(markup.includes('aria-label="添加文本文件或 PDF"'), false);
  assert.match(markup, /aria-label="发送消息"[^>]+title="请先启动运行服务"[^>]+disabled=""/u);
});

test("composer shows real scope and runtime status without opening settings", () => {
  const markup = renderToStaticMarkup(<AgentComposer {...composerProps} status={<span>生产目标 · 等待批准</span>} />);
  assert.ok(markup.includes("生产目标 · 等待批准"));
  assert.ok(markup.includes('aria-label="范围与运行状态"'));
  assert.doesNotMatch(markup, /class="agent-settings-panel/);
  assert.ok(markup.includes("Ctrl+Enter 发送"));
  const archived = renderToStaticMarkup(<AgentComposer {...composerProps} commandContext={{ ...composerProps.commandContext, archived: true }} />);
  assert.match(archived, /aria-label="添加附件"[^>]+disabled=""/);
});

test("expanded preview status never announces a ready or active real runtime", () => {
  const markup = renderToStaticMarkup(<AgentRunStatus preview running runState="executing" tokenUsage={null} agentReady shell />);
  assert.ok(markup.includes("模拟数据 · 发送已停用"));
  assert.doesNotMatch(markup, /已就绪|agent-status-active|agent-status-ready/);
});

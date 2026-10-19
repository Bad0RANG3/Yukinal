import assert from "node:assert/strict";
import test from "node:test";
import { createRef } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { AgentFeed } from "../src/features/agent/AgentFeed.js";

function renderFeed(showJumpControl: boolean): string {
  return renderToStaticMarkup(<AgentFeed
    entries={[{ kind: "assistant", text: "saved response" }]}
    running={false}
    runState={null}
    canSend
    onRetry={() => assert.fail("render must not send")}
    onApproval={async () => { assert.fail("render must not approve"); }}
    pendingApprovalIds={[]}
    approvalStatuses={{}}
    feedRef={createRef<HTMLDivElement>()}
    frameRef={createRef<HTMLDivElement>()}
    onScroll={() => {}}
    showJumpControl={showJumpControl}
    onJumpToLatest={() => assert.fail("render must not move the reading position")}
    motionScope="session"
  />);
}

test("detached conversations expose a keyboard accessible latest button controlling the existing feed", () => {
  const markup = renderFeed(true);
  const feedId = /<div id="([^"]+)" class="agent-feed"/.exec(markup)?.[1];
  assert.ok(feedId);
  assert.ok(markup.includes(`type="button" class="agent-feed-jump" aria-controls="${feedId}"`));
  assert.ok(markup.includes("回到最新"));
  assert.match(markup, /role="region" aria-label="对话内容" tabindex="0"/);
  assert.equal((markup.match(/class="agent-feed"/g) ?? []).length, 1);
});

test("following and initial history do not expose a stale jump action or animate history", () => {
  const markup = renderFeed(false);
  assert.ok(!markup.includes("agent-feed-jump"));
  assert.ok(!markup.includes("is-new-entry"));
  assert.ok(markup.includes("saved response"));
});

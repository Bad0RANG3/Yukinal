import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import { AgentRecentConversations } from "../src/features/agent/AgentRecentConversations.js";

const noop = () => {};
const session = { id: "ses_test", title: "排查数据库连接池", serverId: "srv_db", createdAt: "2026-10-17T00:00:00Z", updatedAt: "2026-10-17T00:00:00Z", messageCount: 2 };
test("AI recents hide an empty history and keep busy sessions unavailable", () => {
  const render = (sessions: typeof session[], busy = false, error = false) => renderToStaticMarkup(<AgentRecentConversations sessions={sessions} busy={busy} error={error} onOpen={noop} onShowAll={noop} onRetry={noop} />);
  assert.equal(render([]), "");
  assert.match(render([session]), /最近对话/);
  assert.match(render([session]), /排查数据库连接池/);
  assert.equal((render([session], true).match(/disabled=""/g) ?? []).length, 2);
  assert.match(render([], false, true), /读取失败.*重试/);
});

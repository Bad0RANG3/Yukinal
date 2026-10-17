import assert from "node:assert/strict";
import test from "node:test";
import type { InvestigationTask, TaskStatus } from "@yukinal/shared";
import { boardCardFeedback, boardPreviewCards, teamBoardCards } from "../src/features/agent-nist/team-board.js";
import type { TeamRun } from "../src/features/agent-nist/team-runtime-store.js";

function task(id: string, status: TaskStatus, serverId = "srv_a"): InvestigationTask {
  return { id, objective: id, status, phase: "investigating", scope: { host: "remote", serverId, environment: "development" },
    mode: "readonly", permissionMode: "ask", automationLevel: "readonly", successCriteria: [], createdBy: "test",
    guardrails: { forbiddenTools: [], forbiddenPathPrefixes: [] }, budget: { maxSteps: 12, maxRunMs: 300000, maxAttempts: 2 },
    createdAt: "2026-10-17T00:00:00Z", updatedAt: "2026-10-17T00:00:00Z" };
}
function run(id: string, extra: Partial<TeamRun> = {}): TeamRun {
  return { id, team: "blue", serverId: "srv_a", goal: id, stage: "thinking", startedAt: 1, updatedAt: 2, ...extra };
}
test("the board groups durable states without inferring a team from task wording", () => {
  const cards = teamBoardCards([task("pending", "pending"), task("working", "executing"), task("waiting", "waiting_user"), task("failed", "failed")], []);
  for (const [id, column] of [["pending", "pending"], ["working", "working"], ["waiting", "waiting"], ["failed", "ended"]]) {
    assert.equal(cards.find((card) => card.id === `task:${id}`)?.column, column);
  }
  assert.equal(cards.every((card) => !card.team), true);
  assert.equal(cards.find((card) => card.failed)?.status, "失败");
});
test("joining attempts deduplicates tasks but a completed model run cannot complete a workflow", () => {
  const cards = teamBoardCards([task("a", "verifying")], [run("a1", { taskId: "a", stage: "completed" }), run("b1", { serverId: "srv_b", sessionId: "ses_b" })]);
  assert.equal(cards.length, 2);
  const card = cards.find((item) => item.task);
  assert.equal(card?.status, "验证中");
  assert.equal(card?.column, "working");
  assert.equal(card?.team, "blue");
});
test("approvals belong to their own run and server, including two sessions on one host", () => {
  const cards = teamBoardCards([task("a", "investigating")], [
    run("a1", { taskId: "a", sessionId: "ses_a", stage: "waiting_approval", team: "yellow" }),
    run("b1", { sessionId: "ses_b", stage: "running_tool" }),
    run("wrong", { taskId: "a", serverId: "srv_other", team: "red" }),
  ]);
  assert.equal(cards.length, 3);
  assert.equal(cards.find((card) => card.task)?.column, "waiting");
  assert.equal(cards.find((card) => card.task)?.team, "yellow");
  assert.equal(cards.find((card) => card.id === "run:b1")?.column, "working");
  assert.equal(cards.find((card) => card.id === "run:wrong")?.serverId, "srv_other");
});
test("persisted terminal task status wins over a stale active run projection", () => {
  const card = teamBoardCards([task("a", "completed")], [run("a1", { taskId: "a", stage: "running_tool" })])[0];
  assert.equal(card?.status, "已完成");
  assert.equal(card?.column, "ended");
  assert.equal(boardCardFeedback(card!, []), null);
});

test("board feedback follows the exact running card and pauses for user approval", () => {
  const cards = teamBoardCards([], [run("active"), run("waiting", { stage: "waiting_approval", sessionId: "ses_b" }), run("done", { stage: "completed" })]);
  assert.equal(boardCardFeedback(cards.find((card) => card.id === "run:active")!, [])?.phase, "working");
  assert.equal(boardCardFeedback(cards.find((card) => card.id === "run:waiting")!, []), null);
  const cue = { runId: "done", callId: "read", team: "blue" as const, serverId: "srv_a", tool: "server.logs", finished: true, until: 200 };
  const done = cards.find((card) => card.id === "run:done")!;
  assert.equal(boardCardFeedback(done, [cue], 100)?.phase, "recent");
  assert.equal(boardCardFeedback(done, [cue], 200), null);
  assert.equal(boardCardFeedback(done, [{ ...cue, runId: "other-session" }], 100), null);
});

test("preview cards expire independently and never enter saved task or usage projections", () => {
  const cue = { runId: "demo", callId: "logs", team: "blue" as const, serverId: "srv_a", tool: "server.logs", finished: false, until: 200, preview: true };
  const previews = boardPreviewCards([cue, { ...cue, callId: "real", preview: false }], 100);
  assert.equal(previews.length, 1);
  assert.equal(previews[0]?.column, "working");
  assert.equal(previews[0]?.task, undefined);
  assert.equal(previews[0]?.run, undefined);
  assert.equal(boardCardFeedback(previews[0]!, [], 100)?.phase, "working");
  assert.equal(boardPreviewCards([{ ...cue, finished: true }], 100)[0]?.column, "ended");
  assert.equal(boardPreviewCards([cue], 200).length, 0);
});

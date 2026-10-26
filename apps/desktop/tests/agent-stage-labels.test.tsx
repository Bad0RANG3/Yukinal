import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import { AGENT_STAGES } from "@yukinal/shared";
import { AGENT_STAGE_LABEL, agentStageLabel } from "../src/features/agent/stage-labels.js";
import { AgentRunStatus } from "../src/features/agent/AgentHeader.js";

test("every derived stage has exactly one label decision, and intake has none", () => {
  // The stage vocabulary is the runtime's, not a UI invention: a stage added there
  // without a label here would otherwise silently fall back to "thinking".
  assert.deepEqual(Object.keys(AGENT_STAGE_LABEL).sort(), [...AGENT_STAGES].sort());
  assert.equal(AGENT_STAGE_LABEL.intake, null);
  for (const stage of AGENT_STAGES) {
    const label = agentStageLabel(stage);
    if (stage === "intake") assert.equal(label, null);
    else assert.match(label!, /…$/);
  }
  assert.equal(agentStageLabel(null), null);
  assert.equal(agentStageLabel(undefined), null);
});

test("a derived stage replaces the generic working label only while the run works", () => {
  const status = (running: boolean, stage: "evidence" | "change" | null, runState: string) => renderToStaticMarkup(
    <AgentRunStatus running={running} runState={runState} stage={stage} tokenUsage={null} agentReady shell />,
  );
  assert.match(status(true, "evidence", "running_tool"), /取证中…/);
  assert.match(status(true, "change", "running_tool"), /执行变更…/);
  // Finished runs keep the terminal state: a stale stage must not claim work.
  assert.match(status(false, "change", "completed"), /完成/);
  // Without a stage the previous generic label is unchanged for older sidecars.
  assert.match(status(true, null, "thinking"), /思考中…/);
  assert.match(status(true, null, "running_tool"), /执行工具…/);
});

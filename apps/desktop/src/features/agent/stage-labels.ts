/**
 * How a derived operating stage reads in the UI.
 *
 * The stage comes from the runtime (`agent.stage`), computed from executed tool
 * results — not from the model's prose. These labels therefore describe what the
 * agent actually did, and `intake` has no label because nothing has happened yet.
 */
import type { AgentStage } from "@yukinal/shared";

export const AGENT_STAGE_LABEL: Readonly<Record<AgentStage, string | null>> = {
  intake: null,
  evidence: "取证中…",
  diagnosis: "诊断中…",
  plan: "整理方案…",
  change: "执行变更…",
  verify: "验证结果…",
  report: "整理结果…",
};

export function agentStageLabel(stage: AgentStage | null | undefined): string | null {
  return stage ? AGENT_STAGE_LABEL[stage] ?? null : null;
}

/**
 * Permission Engine — ADR 0005.
 *
 * Three risk layers produce facts, this file is the only place that turns facts
 * into a decision:
 *
 *   layer 1  tool declaration   base risk, authored by the tool
 *   layer 2  command analysis   rules over the concrete command
 *   layer 3  target environment  production raises the floor
 *
 * Nothing else may execute: the UI supplies an explicit run mode, while the
 * engine remains the only place that turns that delegation into a ticket.
 */

import { randomUUID } from "node:crypto";

import {
  DEFAULT_AGENT_RUN_MODE,
  defaultPolicyFor,
  isReadOnlyRunMode,
  isSessionGrantable,
  maxRisk,
  tierOf,
  type AgentPermissionMode,
  type AgentRunMode,
  type Environment,
  type PermissionApprovalSource,
  type PermissionDecision,
  type PermissionMode,
  type PermissionPolicy,
  type RiskFact,
  type RiskLevel,
  type ToolDeclaration,
  type ToolTarget,
} from "@yukinal/shared";

import { actionFingerprint } from "../security/action-fingerprint.js";
import { analyzeCommand, extractCommand } from "./command-risk.js";

/** Layer 3: the floor the target environment imposes, independent of the tool. */
export const ENVIRONMENT_RISK_FLOOR: Record<Environment, RiskLevel> = {
  local: "low",
  development: "low",
  staging: "medium",
  production: "high",
  // An unlabelled server is treated like production until the user says otherwise.
  unknown: "high",
};

export interface PermissionRequest {
  declaration: ToolDeclaration;
  target: ToolTarget;
  /** Raw tool input, analysed for embedded shell commands. */
  input: unknown;
  /** Omitted -> the environment's built-in default policy. */
  policy?: PermissionPolicy;
  /** Omitted -> preserve the policy-only behaviour for non-UI callers. */
  permissionMode?: AgentPermissionMode;
  /**
   * Omitted -> `goal` (unconstrained). In `plan` / `readonly` the run may not
   * change anything, so every non-read tier is denied before policy or
   * delegation is consulted.
   */
  mode?: AgentRunMode;
}

/** Grants are scoped to the exact `tool + input + target` action. */
export function grantKey(toolName: string, target: ToolTarget, inputFingerprint = "none"): string {
  const where = target.host === "local" ? "local" : (target.serverId ?? "unknown-server");
  const workspace = target.workspaceId ?? "global";
  const base = `${toolName}@${where}@${workspace}@${target.environment}`;
  return inputFingerprint === "none" ? base : `${base}@${inputFingerprint}`;
}

export class PermissionEngine {
  readonly #grants = new Set<string>();
  readonly #now: () => string;

  constructor(options: { now?: () => string } = {}) {
    this.#now = options.now ?? (() => new Date().toISOString());
  }

  get grantCount(): number {
    return this.#grants.size;
  }

  evaluate(request: PermissionRequest): PermissionDecision {
    const { declaration, target, input } = request;
    const policy = request.policy ?? defaultPolicyFor(target.environment);

    const facts: RiskFact[] = [];

    // ---- layer 1: what the tool says about itself
    facts.push({
      source: "tool",
      level: declaration.risk,
      toolName: declaration.name,
      note: "declared base risk",
    });

    // ---- layer 2: what this concrete call would do
    const commandRisk = analyzeCommand(extractCommand(input));
    if (commandRisk !== undefined && commandRisk.matched.length > 0) {
      facts.push({
        source: "command",
        level: commandRisk.level,
        command: commandRisk.command,
        matched: commandRisk.matched.map((rule) => rule.id),
        note: commandRisk.matched.map((rule) => rule.note).join("; "),
      });
    }

    // ---- layer 3: where it would happen
    const environment = target.environment;
    facts.push({
      source: "environment",
      level: ENVIRONMENT_RISK_FLOOR[environment] ?? "high",
      environment,
      note: `risk floor for environment "${environment}"`,
    });

    // Layers 1+2 describe the action itself. The environment may escalate it, but a
    // purely observational call stays observational even on production -- otherwise
    // 's "Production: READ Auto" row could never be satisfied.
    const intrinsicRisk = maxRisk(declaration.risk, commandRisk?.level ?? "read");
    const environmentLevel: RiskLevel =
      intrinsicRisk === "read" ? "read" : (ENVIRONMENT_RISK_FLOOR[environment] ?? "high");
    const envFact = facts[facts.length - 1];
    if (envFact?.source === "environment") envFact.level = environmentLevel;

    const finalRisk = maxRisk(intrinsicRisk, environmentLevel);
    const tier = tierOf(finalRisk);
    let outcome: PermissionMode = policy.tiers[tier];
    let approvedBy: PermissionApprovalSource | undefined = outcome === "auto" ? "policy" : undefined;
    let reason = `${declaration.name} is ${finalRisk} on ${describeTarget(target)}; policy "${policy.name}" says ${outcome} for tier "${tier}"`;

    // Dangerous and critical actions always need a direct user approval. A custom
    // policy or malformed caller must not turn them into an automatic decision.
    if (tier === "dangerous" && outcome === "auto") {
      outcome = "ask";
      approvedBy = undefined;
      reason = `${describeTarget(target)}: dangerous or critical action cannot be auto-approved`;
    }

    // A read-only run may not change anything. This is enforced here rather than
    // requested in the prompt, so a model that ignores its instructions still
    // cannot write. The denial is placed before policy delegation, the `auto`
    // convenience path and session grants, so none of them can widen it back.
    const runMode = request.mode ?? DEFAULT_AGENT_RUN_MODE;
    if (isReadOnlyRunMode(runMode) && tier !== "read") {
      outcome = "deny";
      approvedBy = undefined;
      reason = `${declaration.name} on ${describeTarget(target)} is a ${tier}-tier action and this run is in ${runMode} mode, which may not change anything`;
    }

    // `auto` is intentionally narrow. It may cover ordinary write-tier work on a
    // resolved remote development or staging target, but it must never waive a human
    // confirmation for local, unknown, production, high-risk, or critical work.
    // The remote restriction matches the host's plan-side check (ADR 0073): a local
    // `development` target is still the machine the user is sitting at.
    // Policy denial remains absolute in every mode.
    const agentMayAutoApprove =
      tier === "write" &&
      target.host === "remote" &&
      (target.environment === "development" || target.environment === "staging");
    if (request.permissionMode === "auto" && outcome !== "deny" && agentMayAutoApprove) {
      outcome = "auto";
      approvedBy = "agent";
      reason = `${declaration.name} on ${describeTarget(target)} was delegated to the Agent within the write-tier development/staging boundary; ${reason}`;
    } else if (request.permissionMode === "auto" && outcome !== "deny" && tier !== "read") {
      outcome = "ask";
      approvedBy = undefined;
      reason = `${declaration.name} on ${describeTarget(target)} requires explicit user approval outside the Agent auto-approval boundary; ${reason}`;
    }

    // Ask mode keeps safe reads frictionless but pauses before every state
    // changing or dangerous operation, even when the environment policy says auto.
    if (request.permissionMode === "ask" && outcome === "auto" && tier !== "read") {
      outcome = "ask";
      approvedBy = undefined;
      reason = `${declaration.name} on ${describeTarget(target)} is waiting for user approval because Agent mode is "ask"`;
    }

    // Session grants remember one exact action (tool + target + input fingerprint) for
    // the rest of the run. Before ADR 0072 they widened non-dangerous actions only; they
    // may now also cover a `high` action on a development or staging target, because the
    // grant is still a direct user click on that exact call and the fingerprint stops it
    // from being replayed to another container, unit or package. `critical`, and
    // dangerous-tier work on production, unknown or local targets, still re-asks every
    // time: `isSessionGrantable` is the single rule, shared with `grantSession`, the
    // registry and the approval card, so the engine never advertises an approval that
    // execution will refuse.
    if (
      outcome === "ask" &&
      isSessionGrantable({ tier, finalRisk, target }) &&
      this.#grants.has(grantKey(declaration.name, target, actionFingerprint(input)))
    ) {
      outcome = "auto";
      // `approvedBy` is the provenance the rest of the pipeline dispatches on,
      // so it has to be stamped here. Leaving it undefined sent the call down
      // the `policy_auto` branch in the runtime, which the ToolRegistry then
      // rejected with "Policy auto ticket has no policy authorization" — so a
      // session approval auto-denied the very next identical call.
      approvedBy = "user";
      reason = `${declaration.name} on ${describeTarget(target)} was approved for this session`;
    }

    const decision: PermissionDecision = {
      outcome,
      intrinsicRisk,
      finalRisk,
      tier,
      facts,
      policyId: policy.id,
      toolName: declaration.name,
      inputFingerprint: actionFingerprint(input),
      reason,
      approvedBy,
      target: { ...target },
      requestedAt: this.#now(),
    };

    if (outcome === "ask") {
      decision.approvalId = `apr_${randomUUID()}`;
    }
    return decision;
  }

  /**
   * Called when the user chooses "approve for this run". `approve_once` must not
   * call this. A decision `isSessionGrantable` rejects (critical anywhere, dangerous
   * tier outside development/staging) is never recorded.
   *
   * The rule is judged on `decision.tier` / `decision.finalRisk`, the values after
   * environment escalation, because those are what the execution chokepoint
   * enforces. Recording a grant the registry will always refuse would leave a dead
   * entry in the grant set and make the engine's own decisions misleading.
   */
  grantSession(decision: PermissionDecision): void {
    if (!isSessionGrantable(decision)) return;
    this.#grants.add(grantKey(decision.toolName, decision.target, decision.inputFingerprint));
  }

  /**
   * Drop every session grant.
   *
   * The AgentLoop calls this once no run is in flight. Without it the grant set
   * lives as long as the sidecar process does, so an approval the user gave for
   * one run would silently keep authorising later, unrelated runs.
   */
  clearGrants(): void {
    this.#grants.clear();
  }
}

function describeTarget(target: ToolTarget): string {
  if (target.host === "local") return `local machine (${target.environment})`;
  return `${target.serverId ?? "unresolved server"} (${target.environment})`;
}

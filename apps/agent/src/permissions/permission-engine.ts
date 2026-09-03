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
 * Nothing else may decide: not the LLM, not the UI, not the tool itself.
 */

import { randomUUID } from "node:crypto";

import {
  defaultPolicyFor,
  maxRisk,
  tierOf,
  type Environment,
  type PermissionDecision,
  type PermissionMode,
  type PermissionPolicy,
  type RiskFact,
  type RiskLevel,
  type ToolDeclaration,
  type ToolTarget,
} from "@yukinal/shared";

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
}

/** Grants are scoped to `tool + target`, never to a name the model typed. */
export function grantKey(toolName: string, target: ToolTarget): string {
  const where = target.host === "local" ? "local" : (target.serverId ?? "unknown-server");
  return `${toolName}@${where}@${target.environment}`;
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
    let reason = `${declaration.name} is ${finalRisk} on ${describeTarget(target)}; policy "${policy.name}" says ${outcome} for tier "${tier}"`;

    // A critical call can never be auto-approved by configuration alone
    // (dangerous actions must never be hidden,: rules are not the boundary).
    if (finalRisk === "critical" && outcome === "auto") {
      outcome = "ask";
      reason = `${describeTarget(target)}: critical risk action cannot be auto-approved even if the policy allows it`;
    }

    // Session grants widen non-dangerous actions only: an intrinsically dangerous
    // tool (docker.stop, rm -rf) re-asks every time, wherever it runs.
    if (
      outcome === "ask" &&
      tierOf(intrinsicRisk) !== "dangerous" &&
      finalRisk !== "critical" &&
      this.#grants.has(grantKey(declaration.name, target))
    ) {
      outcome = "auto";
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
      reason,
      target: { ...target },
      requestedAt: this.#now(),
    };

    if (outcome === "ask") {
      decision.approvalId = `apr_${randomUUID()}`;
    }
    return decision;
  }

  /**
   * Called when the user chooses "approve for this session". `approve_once` must not
   * call this. Dangerous tier is never granted for a session.
   */
  grantSession(decision: PermissionDecision): void {
    // The *action* decides, not the environment: production escalation must not
    // permanently lock a routine write once the user has approved it for this session.
    if (tierOf(decision.intrinsicRisk) === "dangerous") return;
    if (decision.finalRisk === "critical") return;
    this.#grants.add(grantKey(decision.toolName, decision.target));
  }

  clearGrants(): void {
    this.#grants.clear();
  }
}

function describeTarget(target: ToolTarget): string {
  if (target.host === "local") return `local machine (${target.environment})`;
  return `${target.serverId ?? "unresolved server"} (${target.environment})`;
}

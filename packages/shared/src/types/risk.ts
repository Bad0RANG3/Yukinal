/**
 * Environment + Risk + Permission contracts.
 *
 * Per ADR 0005: the three risk layers produce *facts*, and a fact is never a decision.
 * The Permission Engine is the only place allowed to turn facts into a decision.
 */

export const RISK_LEVELS = ["read", "low", "medium", "high", "critical"] as const;
export type RiskLevel = (typeof RISK_LEVELS)[number];

export function riskRank(level: RiskLevel): number {
  return RISK_LEVELS.indexOf(level);
}

export function maxRisk(a: RiskLevel, b: RiskLevel): RiskLevel {
  return riskRank(a) >= riskRank(b) ? a : b;
}

/** Target environment (). */
export const ENVIRONMENTS = [
  "local",
  "development",
  "staging",
  "production",
  "unknown",
] as const;
export type Environment = (typeof ENVIRONMENTS)[number];

/**
 * Permission tiers (is written as READ / WRITE / DANGEROUS).
 * Risk levels collapse into tiers so the policy table stays readable.
 */
export const PERMISSION_TIERS = ["read", "write", "dangerous"] as const;
export type PermissionTier = (typeof PERMISSION_TIERS)[number];

export function tierOf(level: RiskLevel): PermissionTier {
  if (level === "read" || level === "low") return "read";
  if (level === "medium") return "write";
  return "dangerous";
}

export const PERMISSION_MODES = ["auto", "ask", "deny"] as const;
export type PermissionMode = (typeof PERMISSION_MODES)[number];

export interface PermissionPolicy {
  id: string;
  name: string;
  environment: Environment;
  /** What happens for each tier in this environment. */
  tiers: Record<PermissionTier, PermissionMode>;
  /** Built-in policy or user-defined. */
  builtin: boolean;
}

/** Built-in defaults, straight from table. */
export const DEVELOPMENT_POLICY: PermissionPolicy = {
  id: "policy.development",
  name: "Development",
  environment: "development",
  tiers: { read: "auto", write: "auto", dangerous: "ask" },
  builtin: true,
};

export const STAGING_POLICY: PermissionPolicy = {
  id: "policy.staging",
  name: "Staging",
  environment: "staging",
  tiers: { read: "auto", write: "auto", dangerous: "ask" },
  builtin: true,
};

export const PRODUCTION_POLICY: PermissionPolicy = {
  id: "policy.production",
  name: "Production",
  environment: "production",
  // WRITE asks, DANGEROUS always asks. makes production identity explicit.
  tiers: { read: "auto", write: "ask", dangerous: "ask" },
  builtin: true,
};

export const LOCAL_POLICY: PermissionPolicy = {
  id: "policy.local",
  name: "Local machine",
  environment: "local",
  tiers: { read: "auto", write: "ask", dangerous: "ask" },
  builtin: true,
};

export function defaultPolicyFor(environment: Environment): PermissionPolicy {
  switch (environment) {
    case "local":
      return LOCAL_POLICY;
    case "development":
      return DEVELOPMENT_POLICY;
    case "staging":
      return STAGING_POLICY;
    case "production":
      return PRODUCTION_POLICY;
    case "unknown":
      // Unknown environment must never be more permissive than production.
      return PRODUCTION_POLICY;
  }
}

/**
 * Layer 1 — static risk declared by the Tool itself (-R6).
 */
export interface ToolRiskFact {
  source: "tool";
  level: RiskLevel;
  toolName: string;
  note?: string;
}

/**
 * Layer 2 — dynamic risk from analysing the concrete command / arguments.
 * `matched` holds rule ids such as `rm-rf`, `drop-database`.
 */
export interface CommandRiskFact {
  source: "command";
  level: RiskLevel;
  command: string;
  matched: string[];
  note?: string;
}

/**
 * Layer 3 — risk contributed by the target environment.
 */
export interface EnvironmentRiskFact {
  source: "environment";
  level: RiskLevel;
  environment: Environment;
  note?: string;
}

export type RiskFact = ToolRiskFact | CommandRiskFact | EnvironmentRiskFact;

export const RISK_FACT_SOURCES = ["tool", "command", "environment"] as const;

/**
 * Output of the Permission Engine. The only authority that may allow execution
 * (the LLM cannot decide this for itself).
 */
export interface PermissionDecision {
  outcome: PermissionMode;
  /** Risk from layers 1+2 alone: what the action *is*. */
  intrinsicRisk: RiskLevel;
  /** Risk after the target environment is taken into account: what this means *here*. */
  finalRisk: RiskLevel;
  /** Policy tier / approval prompt are keyed off `finalRisk`. */
  tier: PermissionTier;
  facts: RiskFact[];
  policyId: string;
  /** The decision is bound to exactly one tool; the registry refuses a mismatch. */
  toolName: string;
  /** Human readable, rendered verbatim in the Approval UI. */
  reason: string;
  /** Resolved stable target, never a free-text name. */
  target: {
    host: "local" | "remote";
    serverId?: string;
    workspaceId?: string;
    environment: Environment;
  };
  /** Present iff outcome === "ask". */
  approvalId?: string;
  requestedAt: string;
}

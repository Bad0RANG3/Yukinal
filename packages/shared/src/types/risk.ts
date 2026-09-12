/**
 * Environment + Risk + Permission contracts.
 *
 * Per ADR 0005: the three risk layers produce *facts*, and a fact is never a decision.
 * The Permission Engine is the only place allowed to turn facts plus an explicit
 * run delegation into an executable decision.
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

/** User-selected delegation for one Agent run. Policy denial always wins. */
export const AGENT_PERMISSION_MODES = ["ask", "auto"] as const;
export type AgentPermissionMode = (typeof AGENT_PERMISSION_MODES)[number];

/**
 * What a run is allowed to accomplish at all. Deliberately a separate axis from
 * `AgentPermissionMode`: the run mode bounds *scope* (may this run change
 * anything?), while the permission mode decides *who approves* the subset that
 * is still permitted. A user can therefore ask for "plan mode, auto-approve"
 * without one setting silently reinterpreting the other.
 */
export const AGENT_RUN_MODES = ["goal", "plan", "readonly"] as const;
export type AgentRunMode = (typeof AGENT_RUN_MODES)[number];

/**
 * Modes that may not change anything on a target. The permission engine turns
 * every non-read tier into a hard `deny` in these modes, so the restriction is
 * enforced rather than merely requested in the prompt — a model that ignores
 * its instructions still cannot write.
 */
export const READ_ONLY_RUN_MODES: readonly AgentRunMode[] = ["plan", "readonly"];

export function isReadOnlyRunMode(mode: AgentRunMode): boolean {
  return READ_ONLY_RUN_MODES.includes(mode);
}

/** The unconstrained mode: the run may do anything the policy and approvals allow. */
export const DEFAULT_AGENT_RUN_MODE: AgentRunMode = "goal";

/** The authority that made an automatic execution possible. */
export const PERMISSION_APPROVAL_SOURCES = ["user", "policy", "agent"] as const;
export type PermissionApprovalSource = (typeof PERMISSION_APPROVAL_SOURCES)[number];

export interface PermissionPolicy {
  id: string;
  name: string;
  environment: Environment;
  /** What happens for each tier in this environment. */
  tiers: Record<PermissionTier, PermissionMode>;
  /** Built-in policy or user-defined. */
  builtin: boolean;
}

/**
 * Built-in defaults, straight from table.
 *
 * `as const satisfies PermissionPolicy` rather than `: PermissionPolicy`, so the
 * `id` stays a literal. That literal is what makes `BuiltinPolicyId` a real union
 * instead of `string`, which in turn is what turns a *missing* label or a *misspelled*
 * policy id in a `Record<BuiltinPolicyId, …>` into a compile error. Widening the
 * annotation back to `PermissionPolicy` silently re-opens that hole.
 */
export const DEVELOPMENT_POLICY = {
  id: "policy.development",
  name: "Development",
  environment: "development",
  tiers: { read: "auto", write: "auto", dangerous: "ask" },
  builtin: true,
} as const satisfies PermissionPolicy;

export const STAGING_POLICY = {
  id: "policy.staging",
  name: "Staging",
  environment: "staging",
  tiers: { read: "auto", write: "auto", dangerous: "ask" },
  builtin: true,
} as const satisfies PermissionPolicy;

export const PRODUCTION_POLICY = {
  id: "policy.production",
  name: "Production",
  environment: "production",
  // WRITE asks, DANGEROUS always asks. makes production identity explicit.
  tiers: { read: "auto", write: "ask", dangerous: "ask" },
  builtin: true,
} as const satisfies PermissionPolicy;

export const LOCAL_POLICY = {
  id: "policy.local",
  name: "Local machine",
  environment: "local",
  tiers: { read: "auto", write: "ask", dangerous: "ask" },
  builtin: true,
} as const satisfies PermissionPolicy;

/**
 * The four built-in policies, and the id union derived from them.
 *
 * This tuple — not a hand-written list of id strings — is what a registry, a
 * `system.describe` reply or a UI picker iterates, so an id cannot exist here and
 * be missing from the policy object it names.
 */
export const BUILTIN_POLICIES = [
  LOCAL_POLICY,
  DEVELOPMENT_POLICY,
  STAGING_POLICY,
  PRODUCTION_POLICY,
] as const;

export type BuiltinPolicyId = (typeof BUILTIN_POLICIES)[number]["id"];

export const BUILTIN_POLICY_IDS: readonly BuiltinPolicyId[] = BUILTIN_POLICIES.map(
  (policy) => policy.id,
);

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
 * Output of the Permission Engine. The engine remains the only execution authority;
 * an explicit run mode may delegate an allowed decision to the Agent, but model text
 * can never forge a user approval or bypass a policy denial.
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
  /** Present for automatic decisions so the audit trail can explain who delegated it. */
  approvedBy?: PermissionApprovalSource;
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

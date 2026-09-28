import { TASK_FAILURE_CODES, type TaskFailureCode } from "./investigation.js";
import { TOOL_ERROR_CODES, type ToolError } from "./tool.js";

/**
 * The canonical, user-actionable error vocabulary.
 *
 * `TaskFailureCode` and `ToolError.code` are wire codes: many of them, shaped by
 * the layer that produced them. Neither is a good thing for a screen to branch
 * on — matching English text is worse — so every code maps into one of these
 * categories, and the category is what a next-step decision is allowed to use.
 *
 * The category is deliberately about *what the user can do next*, not about
 * which subsystem failed. Two codes that read the same on screen (a transient
 * connection drop and a retryable timeout) share a category; two codes that both
 * mention "approval" do too, because both end with the user deciding.
 *
 * `packages/shared/fixtures/error-taxonomy.json` is the single canonical mapping
 * and is asserted by both this package and `crates/database` so the Rust host
 * and the UI cannot drift.
 */
export const ERROR_CATEGORIES = [
  "input",
  "permission",
  "approval",
  "authentication",
  "transport",
  "timeout",
  "cancelled",
  "budget",
  "not_found",
  "unsupported",
  "remote_failure",
  "output",
  "evidence",
  "stale",
  "plan",
  "internal",
  "unknown",
] as const;
export type ErrorCategory = (typeof ERROR_CATEGORIES)[number];

export type ToolErrorCode = ToolError["code"];

const TASK_FAILURE_CATEGORY: Record<TaskFailureCode, ErrorCategory> = {
  budget_exhausted: "budget",
  timeout: "timeout",
  cancelled: "cancelled",
  approval_required: "approval",
  approval_rejected: "approval",
  authentication: "authentication",
  transport: "transport",
  target_not_found: "not_found",
  permission_denied: "permission",
  invalid_input: "input",
  plan_deviation: "plan",
  command_failed: "remote_failure",
  output_truncated: "output",
  evidence_missing: "evidence",
  stale_target: "stale",
  unsupported: "unsupported",
  internal: "internal",
  unknown: "unknown",
};

const TOOL_ERROR_CATEGORY: Record<ToolErrorCode, ErrorCategory> = {
  invalid_input: "input",
  plan_deviation: "plan",
  denied_by_policy: "permission",
  approval_rejected: "approval",
  approval_timeout: "approval",
  timeout: "timeout",
  cancelled: "cancelled",
  not_found: "not_found",
  transport: "transport",
  unsupported: "unsupported",
  execution_failed: "remote_failure",
  internal: "internal",
};

/**
 * The categories a bare retry with the same input may succeed in. This is the
 * host-owned retryability rule for durable tasks; a `ToolError` may carry its own
 * `retryable` flag, but the UI never infers retryability from message text.
 */
const RETRYABLE_TASK_FAILURES: ReadonlySet<TaskFailureCode> = new Set([
  "timeout",
  "transport",
  "authentication",
]);

export function errorCategoryFromTaskFailure(code: TaskFailureCode): ErrorCategory {
  return TASK_FAILURE_CATEGORY[code];
}

export function errorCategoryFromToolError(code: ToolErrorCode): ErrorCategory {
  return TOOL_ERROR_CATEGORY[code];
}

/** Whether a durable-task failure of this code may be retried unchanged. */
export function isRetryableTaskFailure(code: TaskFailureCode): boolean {
  return RETRYABLE_TASK_FAILURES.has(code);
}

/** Whether a category is one where an unchanged retry is the default next step. */
export function isRetryableCategory(category: ErrorCategory): boolean {
  return category === "transport" || category === "timeout" || category === "authentication";
}

// Compile-time exhaustion: adding a wire code without a category stops the build.
const _taskFailureCategoriesCovered: readonly TaskFailureCode[] = TASK_FAILURE_CODES;
const _toolErrorCategoriesCovered: readonly ToolErrorCode[] = TOOL_ERROR_CODES;
void _taskFailureCategoriesCovered;
void _toolErrorCategoriesCovered;

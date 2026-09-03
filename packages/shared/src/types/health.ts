/**
 * Health thresholds — the single vocabulary for "is this server ok?".
 *
 * The canonical values live in `packages/shared/fixtures/health_thresholds.json`
 * (single source consumed by the Rust core via include_str!); this const is pinned
 * to that JSON by `health.test.ts`, so TS and Rust can never disagree silently.
 *
 * Rule: raw numbers become health classes exactly once, here, and in the Rust
 * mirror (`crates/core/src/health.rs`) — UI and Agent never compute it differently.
 */

export const HEALTH_THRESHOLDS = {
  cpu: { warning: 70, critical: 90 },
  memory: { warning: 70, critical: 90 },
  disk: { warning: 75, critical: 90 },
} as const;

export type HealthClass = "healthy" | "warning" | "critical";

/** usagePercent → health class (same arithmetic as the Rust mirror). */
export function healthClass(
  usagePercent: number,
  thresholds: { warning: number; critical: number } = HEALTH_THRESHOLDS.cpu,
): HealthClass {
  if (usagePercent >= thresholds.critical) return "critical";
  if (usagePercent >= thresholds.warning) return "warning";
  return "healthy";
}
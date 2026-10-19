/**
 * Health thresholds — the single vocabulary for "is this server ok?".
 *
 * The canonical values live in `packages/shared/fixtures/health_thresholds.json`. Two
 * readers keep it honest: `health.test.ts` pins this const to the JSON, and the Python
 * agent's own copy is asserted equal to the same file
 * (`agent/tests/test_system_snapshot.py:109-113`). There is no C++ consumer today — the
 * host validates the *class* strings it is handed, not the numbers — so the earlier claim
 * that the Rust core compiled this in with `include_str!` described a host that no longer
 * exists.
 *
 * Rule: raw numbers become health classes exactly once per language, and both sides are
 * pinned to this fixture rather than to each other.
 */

export const HEALTH_THRESHOLDS = {
  cpu: { warning: 70, critical: 90 },
  memory: { warning: 70, critical: 90 },
  disk: { warning: 75, critical: 90 },
} as const;

export type HealthClass = "healthy" | "warning" | "critical";

/** usagePercent → health class (the same arithmetic the Python agent uses). */
export function healthClass(
  usagePercent: number,
  thresholds: { warning: number; critical: number } = HEALTH_THRESHOLDS.cpu,
): HealthClass {
  if (usagePercent >= thresholds.critical) return "critical";
  if (usagePercent >= thresholds.warning) return "warning";
  return "healthy";
}
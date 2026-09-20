/** Stable, non-reversible fingerprints for binding approvals to exact inputs. */

import { createHash } from "node:crypto";

/**
 * Keep object key ordering independent of how a provider assembled the JSON value.
 * Rust's serde_json ordering is byte-oriented, so use the same UTF-8 ordering here.
 */
function canonicalize(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canonicalize);
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>)
        .sort(([left], [right]) => Buffer.from(left, "utf8").compare(Buffer.from(right, "utf8")))
        .map(([key, nested]) => [key, canonicalize(nested)]),
    );
  }
  return value;
}

export function actionFingerprint(value: unknown): string {
  const serialized = JSON.stringify(canonicalize(value)) ?? "null";
  return createHash("sha256").update(serialized).digest("hex");
}

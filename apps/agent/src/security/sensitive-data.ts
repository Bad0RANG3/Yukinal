/**
 * Redaction at the boundary between local infrastructure data and model/UI output.
 *
 * This is deliberately conservative. A false positive hides one value; a false
 * negative can send a credential to an external provider or persist it in an audit.
 */

export const REDACTED = "[redacted]";

const SENSITIVE_KEY = /(?:api|access|auth|bearer|client|private|secret|session|service|signing)?(?:key|token|password|passwd|credential|passphrase)|authorization|cookie/i;

const TEXT_REDACTORS: ReadonlyArray<RegExp> = [
  /-----BEGIN(?: [A-Z0-9]+)? PRIVATE KEY-----[\s\S]*?-----END(?: [A-Z0-9]+)? PRIVATE KEY-----/g,
  /\bsk-(?:proj-)?[A-Za-z0-9_-]{8,}\b/g,
  /\b(?:ghp|gho|ghu|ghs)_[A-Za-z0-9_]{20,}\b/g,
  /\bgithub_pat_[A-Za-z0-9_]{20,}\b/g,
  /\bAKIA[0-9A-Z]{16}\b/g,
  /\bAIza[0-9A-Za-z_-]{30,}\b/g,
  /\bxox[baprs]-[A-Za-z0-9-]{12,}\b/g,
  /\b(?:Bearer|Basic)\s+[A-Za-z0-9._~+\/=:-]{8,}\b/gi,
  /\b([A-Za-z][A-Za-z0-9_-]*(?:api|access|auth|client|private|secret|session|service)?(?:key|token|password|passwd|credential|passphrase)|authorization|cookie)\s*[:=]\s*(?:"[^"]*"|'[^']*'|[^\s,;)}\]]+)/gi,
  /\b([a-z][a-z0-9+.-]*:\/\/[^:\s/@]+:)[^@\s/]+@/gi,
];

/** True for object keys that must never have their values emitted verbatim. */
export function isSensitiveKey(key: string): boolean {
  return SENSITIVE_KEY.test(key.replace(/[^a-z0-9]/gi, ""));
}

/** Replace recognizable credentials while retaining enough surrounding text for diagnostics. */
export function redactSensitiveText(value: string): string {
  let redacted = value;
  for (const pattern of TEXT_REDACTORS) {
    redacted = redacted.replace(pattern, (_match, prefix: string | undefined) =>
      prefix === undefined ? REDACTED : `${prefix}${REDACTED}`,
    );
  }
  return redacted;
}

/** Clone arbitrary JSON-like data with sensitive property values and strings redacted. */
export function redactSensitiveValue(value: unknown, seen = new WeakSet<object>()): unknown {
  if (typeof value === "string") return redactSensitiveText(value);
  if (typeof value !== "object" || value === null) return value;
  if (seen.has(value)) return "[circular]";
  seen.add(value);
  if (Array.isArray(value)) return value.map((item) => redactSensitiveValue(item, seen));

  return Object.fromEntries(
    Object.entries(value as Record<string, unknown>).map(([key, candidate]) => [
      key,
      isSensitiveKey(key) ? REDACTED : redactSensitiveValue(candidate, seen),
    ]),
  );
}

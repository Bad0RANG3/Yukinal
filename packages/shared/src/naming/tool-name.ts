/**
 * Tool name mapping — ADR 0004.
 *
 * Internally (registry, trace, audit, DB, IPC) tools are dot-namespaced: `docker.ps`.
 * At the LLM boundary they are double-underscored:                `docker__ps`.
 *
 * Why: most OpenAI-compatible endpoints restrict function names to
 * `^[a-zA-Z0-9_-]{1,64}$`. Dots are rejected or silently normalised by gateways,
 * and a silently-renamed tool is an unauditable tool.
 */

export const INTERNAL_SEPARATOR = ".";
export const PROVIDER_SEPARATOR = "__";

/** OpenAI-compatible function-name charset + length limit. */
export const PROVIDER_TOOL_NAME_PATTERN = /^[a-zA-Z0-9_-]{1,64}$/;
export const PROVIDER_TOOL_NAME_MAX_LENGTH = 64;

const SEGMENT_PATTERN = /^[a-z][a-z0-9]*(-[a-z0-9]+)*$/;

/** `docker.ps` ok. `docker` / `Docker.ps` / `a__b` / `.ps` rejected. */
export function isValidInternalToolName(name: string): boolean {
  if (name.includes(PROVIDER_SEPARATOR)) return false;
  const segments = name.split(INTERNAL_SEPARATOR);
  // Names are always `namespace.action`: a bare namespace hides intent.
  if (segments.length < 2) return false;
  return segments.every((segment) => SEGMENT_PATTERN.test(segment));
}

export function toProviderToolName(internalName: string): string {
  if (!isValidInternalToolName(internalName)) {
    throw new InvalidToolNameError(internalName);
  }
  const providerName = internalName.split(INTERNAL_SEPARATOR).join(PROVIDER_SEPARATOR);
  if (providerName.length > PROVIDER_TOOL_NAME_MAX_LENGTH) {
    throw new ToolNameTooLongError(internalName, providerName.length);
  }
  return providerName;
}

export function fromProviderToolName(providerName: string): string {
  if (!PROVIDER_TOOL_NAME_PATTERN.test(providerName)) {
    throw new InvalidToolNameError(providerName);
  }
  return providerName.split(PROVIDER_SEPARATOR).join(INTERNAL_SEPARATOR);
}

/**
 * Our own namespace is injective by construction, but adapters (MCP, phase 3) bring
 * foreign spellings such as `docker__get` that project onto the same provider name as
 * `docker.get`. This check is therefore deliberately *lenient*: it projects without
 * validating, so a shadowed tool is caught at registration instead of at call time.
 * Validation itself stays in `toProviderToolName` / `isValidInternalToolName`.
 */
export function assertUniqueProviderNames(internalNames: readonly string[]): void {
  const seen = new Map<string, string>();
  for (const internalName of internalNames) {
    const providerName = internalName.split(INTERNAL_SEPARATOR).join(PROVIDER_SEPARATOR);
    const existing = seen.get(providerName);
    if (existing !== undefined && existing !== internalName) {
      throw new ToolNameCollisionError(existing, internalName, providerName);
    }
    seen.set(providerName, internalName);
  }
}

export class InvalidToolNameError extends Error {
  constructor(name: string) {
    super(
      `Invalid tool name "${name}". Expected dot-namespaced lowercase segments, e.g. "docker.ps".`,
    );
    this.name = "InvalidToolNameError";
  }
}

export class ToolNameTooLongError extends Error {
  constructor(name: string, length: number) {
    super(
      `Tool name "${name}" maps to ${length} provider characters; the limit is ${PROVIDER_TOOL_NAME_MAX_LENGTH}.`,
    );
    this.name = "ToolNameTooLongError";
  }
}

export class ToolNameCollisionError extends Error {
  constructor(a: string, b: string, providerName: string) {
    super(`Tool names "${a}" and "${b}" both map to provider name "${providerName}".`);
    this.name = "ToolNameCollisionError";
  }
}

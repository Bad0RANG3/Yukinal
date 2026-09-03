/**
 * The one and only place where internal tool names are rewritten for providers
 * (ADR 0004). The agent loop and every tool implementation keep using dots.
 */

import { toProviderToolName, type JsonSchema, type ToolDeclaration } from "@yukinal/shared";

import type { ProviderToolSpec } from "./types.js";

export interface ProviderNameIndex {
  /** `docker.ps` -> `docker__ps`. Throws for unknown tools. */
  providerFor(internalName: string): string;
  /** `docker__ps` -> `docker.ps`. Returns undefined for names we never advertised. */
  internalFor(providerName: string): string | undefined;
  /** Specs in registration order — deterministic output keeps prompts cacheable. */
  specs(): ProviderToolSpec[];
  size(): number;
}

export function buildToolSpec(declaration: ToolDeclaration): ProviderToolSpec {
  const parameters: JsonSchema = declaration.inputSchema;
  return {
    type: "function",
    function: {
      name: toProviderToolName(declaration.name),
      description: declaration.description,
      parameters,
    },
  };
}

export function createProviderNameIndex(declarations: readonly ToolDeclaration[]): ProviderNameIndex {
  const toProvider = new Map<string, string>();
  const toInternal = new Map<string, string>();
  const specs: ProviderToolSpec[] = [];

  for (const declaration of declarations) {
    const spec = buildToolSpec(declaration);
    const existing = toInternal.get(spec.function.name);
    if (existing !== undefined) {
      throw new Error(
        `Tool name collision at the provider boundary: "${existing}" and "${declaration.name}" both map to "${spec.function.name}"`,
      );
    }
    toProvider.set(declaration.name, spec.function.name);
    toInternal.set(spec.function.name, declaration.name);
    specs.push(spec);
  }

  return {
    providerFor(internalName) {
      const mapped = toProvider.get(internalName);
      if (mapped === undefined) {
        throw new Error(`Tool "${internalName}" is not registered for this provider`);
      }
      return mapped;
    },
    internalFor(providerName) {
      return toInternal.get(providerName);
    },
    specs() {
      return specs.map((spec) => ({
        ...spec,
        function: { ...spec.function },
      }));
    },
    size() {
      return specs.length;
    },
  };
}

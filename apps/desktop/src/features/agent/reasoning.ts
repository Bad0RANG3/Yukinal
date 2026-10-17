import type { AgentReasoningEffort, AiProviderConfig } from "@yukinal/shared";

export const REASONING_LABELS: Record<AgentReasoningEffort, string> = { none: "快速", low: "轻量", high: "深入", max: "Max" };
const DEEPSEEK_LEVELS: readonly AgentReasoningEffort[] = ["none", "low", "high", "max"];

/** Only offer parameters verified against the selected endpoint's documented API. */
export function reasoningLevels(provider: AiProviderConfig | undefined, model: string | null | undefined): readonly AgentReasoningEffort[] {
  if (!provider || provider.kind !== "openai-compatible" || provider.wireApi === "responses") return [];
  try {
    const url = new URL(provider.baseUrl);
    return url.protocol === "https:" && url.hostname === "api.deepseek.com" && /^deepseek-(flash|v4-pro)$/.test(model ?? "") ? DEEPSEEK_LEVELS : [];
  } catch { return []; }
}

export function requestReasoningEffort(provider: AiProviderConfig | undefined, model: string | null | undefined, effort: AgentReasoningEffort | null): AgentReasoningEffort | undefined {
  return effort !== null && reasoningLevels(provider, model).includes(effort) ? effort : undefined;
}

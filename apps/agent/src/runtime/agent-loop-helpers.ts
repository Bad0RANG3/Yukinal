/**
 * Pure or narrowly-scoped helpers for AgentLoop.
 *
 * The main loop owns run lifecycle and the protocol sequence.  This module owns
 * prompt projection, bounded summaries and the small predicates that describe
 * whether a tool participates in a durable plan.  Keeping those rules here
 * makes their interface explicit without moving execution authority away from
 * AgentLoop.
 */

import type {
  AgentAudioPromptPart,
  AgentDocumentPromptPart,
  AgentImagePromptPart,
  AgentRunRequest,
  AgentTextFilePromptPart,
  AgentTextPromptPart,
} from "@yukinal/shared";

import { redactSensitiveText } from "../security/sensitive-data.js";

export function positiveInteger(value: number, name: string): number {
  if (!Number.isInteger(value) || value <= 0) throw new Error(`${name} must be a positive integer`);
  return value;
}

/**
 * The trace ledger's title is the first non-blank, redacted prompt line.  It is
 * intentionally derived from input because failed runs need a recognisable
 * title even when the provider never produced an answer.
 */
export function runTitle(prompt: string): string {
  const firstLine =
    redactSensitiveText(prompt)
      .split("\n")
      .map((line) => line.trim())
      .find((line) => line.length > 0) ?? "";
  if (!firstLine) return "Agent run";
  return firstLine.length > 80 ? `${firstLine.slice(0, 80)}…` : firstLine;
}

export function isRunTimeout(signal: AbortSignal): boolean {
  const reason = signal.reason;
  return reason instanceof Error && reason.message === "run-timeout";
}

/**
 * Wait for the next host-owned observation slot without outliving the run.
 * Malformed timestamps fail closed instead of becoming a tight polling loop.
 */
export function waitForObservationSample(nextSampleAt: string, signal: AbortSignal): Promise<boolean> {
  const dueAt = Date.parse(nextSampleAt);
  if (!Number.isFinite(dueAt)) return Promise.resolve(false);
  const delayMs = Math.max(0, dueAt - Date.now());
  if (delayMs === 0) return Promise.resolve(true);
  return new Promise<boolean>((resolve) => {
    let settled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const finish = (ready: boolean): void => {
      if (settled) return;
      settled = true;
      if (timer !== undefined) clearTimeout(timer);
      signal.removeEventListener("abort", onAbort);
      resolve(ready);
    };
    const onAbort = (): void => finish(false);
    timer = setTimeout(() => finish(true), Math.min(delayMs, 2_147_000_000));
    timer.unref?.();
    if (signal.aborted) finish(false);
    else signal.addEventListener("abort", onAbort, { once: true });
  });
}

/**
 * Investigation metadata is not a fresh target observation, so it must never
 * be persisted as evidence for a scheduled comparison.
 */
export function shouldAutoRecordEvidence(toolName: string): boolean {
  return !toolName.startsWith("investigation.");
}

const PLAN_CONTROL_TOOLS = new Set(["investigation.plan", "investigation.playbook"]);
const LOCAL_PLAN_CONTEXT_TOOLS = new Set([
  "investigation.evidence",
  "investigation.evidence.search",
  "investigation.evidence.compare",
  "investigation.evidence.correlate",
  "investigation.evidence.triage",
]);
const PLAN_NON_ADVANCING_TOOLS = new Set([
  ...PLAN_CONTROL_TOOLS,
  ...LOCAL_PLAN_CONTEXT_TOOLS,
  "investigation.artifact",
]);

/** Plan and local-context tools must not be rejected by an active remote step. */
export function shouldCheckPlan(toolName: string): boolean {
  return !PLAN_CONTROL_TOOLS.has(toolName) && !LOCAL_PLAN_CONTEXT_TOOLS.has(toolName);
}

/** Only a target observation or action can complete the active remote step. */
export function shouldAdvancePlan(toolName: string): boolean {
  return !PLAN_NON_ADVANCING_TOOLS.has(toolName);
}

export function summarize(output: unknown): string {
  if (output === undefined || output === null) return "(no output)";
  const text = redactSensitiveText(typeof output === "string" ? output : JSON.stringify(output));
  return text.length > 400 ? `${text.slice(0, 400)}…` : text;
}

export function agentPromptText(parts: AgentRunRequest["parts"]): string {
  return (
    parts
      ?.filter((part): part is AgentTextPromptPart => part.type === "text")
      .map((part) => part.text)
      .join("\n")
      .trim() ?? ""
  );
}

export function agentPromptImages(parts: AgentRunRequest["parts"]): Array<{
  mediaType: AgentImagePromptPart["mediaType"];
  data: string;
  name?: string;
}> {
  return (
    parts
      ?.filter((part): part is AgentImagePromptPart => part.type === "image")
      .map(({ mediaType, data, name }) => ({ mediaType, data, ...(name ? { name } : {}) })) ?? []
  );
}

export function agentPromptFiles(parts: AgentRunRequest["parts"]): AgentTextFilePromptPart[] {
  return parts?.filter((part): part is AgentTextFilePromptPart => part.type === "file") ?? [];
}

export function agentPromptDocuments(parts: AgentRunRequest["parts"]): Array<{
  mediaType: AgentDocumentPromptPart["mediaType"];
  data: string;
  name: string;
}> {
  return (
    parts
      ?.filter((part): part is AgentDocumentPromptPart => part.type === "document")
      .map(({ mediaType, data, name }) => ({ mediaType, data, name })) ?? []
  );
}

export function agentPromptAudios(parts: AgentRunRequest["parts"]): Array<{
  mediaType: AgentAudioPromptPart["mediaType"];
  data: string;
  name?: string;
}> {
  return (
    parts
      ?.filter((part): part is AgentAudioPromptPart => part.type === "audio")
      .map(({ mediaType, data, name }) => ({ mediaType, data, ...(name ? { name } : {}) })) ?? []
  );
}

export function promptWithTextFiles(prompt: string, files: readonly AgentTextFilePromptPart[]): string {
  if (files.length === 0) return prompt;
  const blocks = files.map(
    (file) =>
      `--- BEGIN ATTACHED TEXT FILE: ${file.name} ---\n${file.data}\n--- END ATTACHED TEXT FILE ---`,
  );
  return [prompt, ...blocks].filter((block) => block.trim().length > 0).join("\n\n");
}

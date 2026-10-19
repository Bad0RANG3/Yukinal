import { RunLifecycle } from "./run-lifecycle.js";
import {
  appendAssistantDelta,
  appendEntries,
  appendReasoningDelta,
  settleAssistantText,
  type Entry,
  type ReplyIdentity,
} from "./transcript.js";

export const STREAM_FLUSH_INTERVAL_MS = 32;
export const MAX_PENDING_STREAM_CHARS = 32_768;
export const MAX_PENDING_STREAM_SEGMENTS = 64;

export type StreamDelta = { kind: "assistant" | "reasoning"; text: string };
export type RunStreamScope = {
  readonly runId: string;
  readonly identity: ReplyIdentity;
  sessionId: string | null;
};

/** One state update per batch, preserving alternating public text/reasoning order. */
export function appendStreamBatch(current: Entry[], deltas: readonly StreamDelta[], identity: ReplyIdentity): Entry[] {
  return deltas.reduce((entries, delta) => delta.kind === "assistant"
    ? appendAssistantDelta(entries, delta.text, identity)
    : appendReasoningDelta(entries, delta.text), current);
}

/** Only calibrate the current model turn; earlier tool-round explanations stay intact. */
export function settleStreamText(current: Entry[], text: string, identity: ReplyIdentity, streamedText: string): Entry[] {
  if (!text) return current;
  if (!streamedText) return settleAssistantText(current, text, identity);
  if (streamedText.startsWith(text)) return current;
  if (text.startsWith(streamedText)) return appendAssistantDelta(current, text.slice(streamedText.length), identity);
  // A divergent terminal snapshot is evidence too; never silently erase visible text.
  return appendEntries(current, [{ kind: "assistant", text, ...identity }]);
}

/**
 * A bounded, run-owned text buffer, independent of React and the desktop transport.
 * Timers improve paint frequency, never event order: structural events flush first.
 * A scope is an object identity so even a retry using the same runId cannot accept an
 * earlier start/stop/approval promise. Reset discards only this view's pending work.
 */
export class AgentRunStream {
  private readonly lifecycle = new RunLifecycle();
  private scope: RunStreamScope | null = null;
  private pending: { kind: StreamDelta["kind"]; parts: string[] }[] = [];
  private pendingChars = 0;
  private streamedText = "";
  private timer: ReturnType<typeof setTimeout> | null = null;
  private timerToken: object | null = null;

  constructor(private readonly emit: (deltas: readonly StreamDelta[], identity: ReplyIdentity) => void) {}

  begin(runId: string, identity: ReplyIdentity): RunStreamScope | null {
    if (!this.lifecycle.begin(runId)) return null;
    this.clearPending();
    this.streamedText = "";
    this.scope = {
      runId,
      identity: {
        ...(identity.participant ? { participant: { ...identity.participant } } : {}),
        ...(identity.model ? { model: identity.model } : {}),
      },
      sessionId: null,
    };
    return this.scope;
  }

  scopeFor(runId: string): RunStreamScope | null {
    return this.scope?.runId === runId ? this.scope : null;
  }

  isCurrent(scope: RunStreamScope): boolean {
    return this.scope === scope;
  }

  isPending(scope: RunStreamScope): boolean {
    return this.isCurrent(scope) && this.lifecycle.running;
  }

  isActive(runId: string): boolean {
    return this.scopeFor(runId) !== null && this.lifecycle.isActive(runId);
  }

  started(runId: string): boolean {
    return this.scopeFor(runId) !== null && this.lifecycle.started(runId);
  }

  acknowledge(scope: RunStreamScope): boolean {
    return this.isPending(scope) && this.lifecycle.acknowledge(scope.runId);
  }

  enqueue(runId: string, kind: StreamDelta["kind"], text: string): void {
    const scope = this.scopeFor(runId);
    if (!scope || !this.isActive(runId) || !text) return;
    if (text.length >= MAX_PENDING_STREAM_CHARS) {
      this.flush();
      this.publish([{ kind, text }], scope);
      return;
    }
    if (this.pendingChars + text.length > MAX_PENDING_STREAM_CHARS) this.flush();
    const last = this.pending.at(-1);
    if (last?.kind === kind) last.parts.push(text);
    else this.pending.push({ kind, parts: [text] });
    this.pendingChars += text.length;
    if (this.pendingChars >= MAX_PENDING_STREAM_CHARS || this.pending.length >= MAX_PENDING_STREAM_SEGMENTS) {
      this.flush();
      return;
    }
    if (this.timer !== null) return;
    const token = {};
    this.timerToken = token;
    this.timer = setTimeout(() => {
      if (this.timerToken !== token || !this.isPending(scope)) return;
      this.timer = null;
      this.timerToken = null;
      this.flush();
    }, STREAM_FLUSH_INTERVAL_MS);
  }

  flush(): void {
    const scope = this.scope;
    const deltas = this.pending.map((delta) => ({ kind: delta.kind, text: delta.parts.join("") }));
    this.clearPending();
    if (scope && this.isPending(scope) && deltas.length) this.publish(deltas, scope);
  }

  /** Tool/approval boundaries separate the preamble from the next model's answer. */
  boundary(): void {
    this.flush();
    this.streamedText = "";
  }

  finish(runId: string): { scope: RunStreamScope; streamedText: string } | null {
    const scope = this.scopeFor(runId);
    if (!scope || !this.isPending(scope)) return null;
    this.flush();
    // Terminal events may precede the start RPC response, or be its sync fallback.
    // Acknowledge only this exact owned run, releasing the admission slot promptly.
    this.lifecycle.acknowledge(runId);
    if (!this.lifecycle.finish(runId)) return null;
    return { scope, streamedText: this.streamedText };
  }

  fail(scope = this.scope): RunStreamScope | null {
    if (!scope || !this.isPending(scope)) return null;
    this.flush();
    this.lifecycle.fail();
    return scope;
  }

  reset(): void {
    this.clearPending();
    this.lifecycle.fail();
    this.scope = null;
    this.streamedText = "";
  }

  private publish(deltas: readonly StreamDelta[], scope: RunStreamScope): void {
    this.streamedText += deltas.filter((delta) => delta.kind === "assistant").map((delta) => delta.text).join("");
    this.emit(deltas, scope.identity);
  }

  private clearPending(): void {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    this.timerToken = null;
    this.pending = [];
    this.pendingChars = 0;
  }
}

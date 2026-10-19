export type CopyStatus = "idle" | "copying" | "copied" | "error";
export type CopySnapshot = Readonly<{ status: CopyStatus; error: string | null; writePending?: boolean }>;
export type ClipboardWriter = (text: string) => Promise<void>;

const IDLE: CopySnapshot = { status: "idle", error: null };
const COPYING: CopySnapshot = { status: "copying", error: null };
const COPIED: CopySnapshot = { status: "copied", error: null };
const COPY_FEEDBACK_MS = 2000;
const COPY_WRITE_TIMEOUT_MS = 10000;
const COPY_FAILURE = "复制失败，请允许剪贴板访问后重试，或选择文本手动复制。";
const COPY_UNAVAILABLE = "当前环境无法访问剪贴板，请选择文本手动复制。";
const COPY_TIMEOUT_PENDING = "系统剪贴板仍未响应，可选择文本手动复制；当前请求结束后才能重试。";
const COPY_TIMEOUT_SETTLED = "复制等待已超时，请重试或选择文本手动复制。";
const WAITING: CopySnapshot = { status: "error", error: COPY_TIMEOUT_PENDING, writePending: true };

class ClipboardWriteError extends Error {
  constructor(readonly unavailable: boolean) {
    super(unavailable ? COPY_UNAVAILABLE : COPY_FAILURE);
    this.name = "ClipboardWriteError";
  }
}

/** Call directly from the click stack so the browser retains user activation. */
export async function writeClipboardText(text: string): Promise<void> {
  try {
    const clipboard = typeof navigator === "undefined" ? undefined : navigator.clipboard;
    if (typeof clipboard?.writeText !== "function") throw new ClipboardWriteError(true);
    await clipboard.writeText(text);
  } catch (error) {
    // Accessors and browser errors may include private context; only fixed messages reach the UI.
    throw error instanceof ClipboardWriteError ? error : new ClipboardWriteError(false);
  }
}

type CopyOutcome = "copied" | "error" | "stale" | "ignored";
type PendingWrite = {
  generation: number;
  timedOut: boolean;
  deadline: ReturnType<typeof setTimeout> | null;
  resolve: (outcome: CopyOutcome) => void;
};

/**
 * Local to one copy control, with one in-flight browser write and no automatic retry.
 * A timeout ends feedback waiting, not the browser write: its lock remains until settlement.
 */
export function createCopyController(initialText: string, write: ClipboardWriter = writeClipboardText, options: { timeoutMs?: number } = {}) {
  const timeoutMs = options.timeoutMs ?? COPY_WRITE_TIMEOUT_MS;
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) throw new RangeError("Clipboard timeout must be positive and finite");
  let text = initialText;
  let active = true;
  let generation = 0;
  let operation: PendingWrite | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let snapshot = IDLE;
  const listeners = new Set<() => void>();

  const clearFeedback = () => {
    if (timer !== null) clearTimeout(timer);
    timer = null;
  };
  const clearDeadline = (attempt: PendingWrite) => {
    if (attempt.deadline !== null) clearTimeout(attempt.deadline);
    attempt.deadline = null;
  };
  const publish = (next: CopySnapshot) => {
    if (!active || (snapshot.status === next.status && snapshot.error === next.error && snapshot.writePending === next.writePending)) return;
    snapshot = next;
    for (const listener of listeners) listener();
  };
  const armDeadline = (attempt: PendingWrite) => {
    attempt.deadline = setTimeout(() => {
      attempt.deadline = null;
      if (!active || operation !== attempt) return;
      attempt.timedOut = true;
      publish(WAITING);
      attempt.resolve(generation === attempt.generation ? "error" : "stale");
    }, timeoutMs);
  };
  const finish = (attempt: PendingWrite, outcome: { ok: true } | { ok: false; error: unknown }) => {
    if (operation !== attempt) return;
    clearDeadline(attempt);
    operation = null;
    if (!active || generation !== attempt.generation) {
      if (active) publish(IDLE);
      attempt.resolve("stale");
    } else if (attempt.timedOut) {
      // A late OS result cannot retroactively turn the timed-out action into visible success.
      publish({ status: "error", error: COPY_TIMEOUT_SETTLED });
      attempt.resolve("error");
    } else if (outcome.ok) {
      publish(COPIED);
      if (active && generation === attempt.generation && operation === null) {
        timer = setTimeout(() => {
          timer = null;
          if (active && generation === attempt.generation && operation === null) publish(IDLE);
        }, COPY_FEEDBACK_MS);
      }
      attempt.resolve("copied");
    } else {
      publish({ status: "error", error: outcome.error instanceof ClipboardWriteError ? outcome.error.message : COPY_FAILURE });
      attempt.resolve("error");
    }
  };

  return {
    getSnapshot: () => snapshot,
    getServerSnapshot: () => IDLE,
    subscribe: (listener: () => void) => {
      listeners.add(listener);
      return () => { listeners.delete(listener); };
    },
    setText(next: string) {
      if (next === text) return;
      text = next;
      generation += 1;
      clearFeedback();
      publish(operation === null ? IDLE : operation.timedOut ? WAITING : COPYING);
    },
    activate() {
      active = true;
      if (operation !== null && operation.deadline === null && !operation.timedOut) armDeadline(operation);
      publish(operation === null ? IDLE : operation.timedOut ? WAITING : COPYING);
    },
    dispose() {
      active = false;
      generation += 1;
      clearFeedback();
      if (operation !== null) {
        clearDeadline(operation);
        operation.resolve("stale");
      }
      snapshot = IDLE;
    },
    copy(): Promise<CopyOutcome> {
      if (!active || operation !== null || text.length === 0) return Promise.resolve("ignored");
      const capturedText = text;
      let resolve!: PendingWrite["resolve"];
      const result = new Promise<CopyOutcome>((done) => { resolve = done; });
      const attempt: PendingWrite = { generation, timedOut: false, deadline: null, resolve };
      operation = attempt;
      clearFeedback();
      publish(COPYING);
      armDeadline(attempt);
      try {
        // No await before this call: writeText runs within the user's click gesture.
        void Promise.resolve(write(capturedText)).then(
          () => finish(attempt, { ok: true }),
          (error: unknown) => finish(attempt, { ok: false, error }),
        );
      } catch (error) {
        finish(attempt, { ok: false, error });
      }
      return result;
    },
  };
}

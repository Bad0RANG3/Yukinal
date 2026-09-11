/**
 * The only way the UI touches native capability (-R9-R10).
 *
 * Design constraints:
 * - `command` is keyed by the shared allow-list, so the UI cannot invent native surface.
 * - params/response types come from `IpcCommandMap`, the single contract both sides
 *   compile against. Rust mirrors the same names and shapes.
 * - When a command starts returning data that the UI branches on, a zod schema from
 *   `@yukinal/shared` is passed to `parse` — parse, never cast.
 * - Events go through `listenDesktop`, gated by `EVENT_SCHEMAS`, so a native
 *   payload is parsed before a component sees it for the same reason.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  EVENT_SCHEMAS,
  IPC_COMMANDS,
  IPC_SCHEMAS,
  tauriEventName,
  type IpcCommandMap,
  type IpcCommandName,
} from "@yukinal/shared";
// 只用它的类型层（`z.output`），所以是 `import type`：这行不会进入运行时产物。
import type { z } from "zod";

/** True when running inside Tauri; false in a plain browser during `vite dev`. */
export function isDesktopShell(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export class IpcUnavailableError extends Error {
  constructor(command: string) {
    super(`请在 Yukinal 桌面应用中执行此操作（${command}）。`);
    this.name = "IpcUnavailableError";
  }
}

export async function callDesktop<C extends IpcCommandName>(
  command: C,
  params: IpcCommandMap[C]["params"],
): Promise<IpcCommandMap[C]["response"]> {
  return callDesktopParsed(command, params, (raw) => IPC_SCHEMAS[command].response.parse(raw));
}

/** Same as `callDesktop`, but validates/normalises the raw payload first. */
export async function callDesktopParsed<C extends IpcCommandName, T>(
  command: C,
  params: IpcCommandMap[C]["params"],
  parse: (raw: unknown) => T,
): Promise<T> {
  if (!isDesktopShell()) throw new IpcUnavailableError(command);
  const validatedParams = IPC_SCHEMAS[command].params.parse(params);
  // These Rust commands accept one structured `input`; the typed UI contract
  // deliberately stays flat. Keep this transport detail at the IPC boundary.
  const args = command === IPC_COMMANDS.serverAdd || command === IPC_COMMANDS.serverUpdate
    ? { input: validatedParams }
    : { ...(validatedParams as Record<string, unknown>) };
  try {
    return parse(await invoke<unknown>(command, args));
  } catch (error) {
    throw error instanceof Error ? error : new Error(String(error));
  }
}

/** Event channels the UI may subscribe to, and the payload each one carries. */
export type DesktopEventName = keyof typeof EVENT_SCHEMAS;

/**
 * The payload type of one channel, resolved **per channel**.
 *
 * This used to be `ReturnType<(typeof EVENT_SCHEMAS)[E]["parse"]>`, and it used to
 * resolve to the union of every payload — `DesktopEventPayload<"agent.completed">` was
 * not the completed event. The cause was **not** this type: it was `EVENT_SCHEMAS`
 * mapping all eight `agent.*` channels to one union schema, because with every channel
 * pointing at the same schema there is nothing for the index to narrow *to*.
 * `ReturnType` and `z.output` both narrow correctly once the channels are per-member —
 * measured against a rebuild rather than assumed, since a stale `packages/shared/dist`
 * made an earlier attempt at this look like a `ReturnType` limitation.
 *
 * `z.output` is kept because it is the repo's existing idiom for exactly this job
 * (a generic schema → its output type): see `Assignable<S extends z.ZodType, …>` in
 * `schemas/consistency.ts`. It also states the intent directly instead of going through
 * whichever method happens to be called `parse`.
 */
export type DesktopEventPayload<E extends DesktopEventName> = z.output<(typeof EVENT_SCHEMAS)[E]>;

/** The subset of channels that carry an `AgentStreamEvent`. */
export type AgentEventName = Extract<DesktopEventName, `agent.${string}`>;

/**
 * Subscribe to a native event through the same gate as commands.
 *
 * Event names are keyed by `EVENT_SCHEMAS`, so the UI cannot subscribe to a
 * channel that is not in the shared contract, and the handler only ever receives
 * a payload that parsed. A payload that fails validation is dropped and reported
 * once per channel name, because an event cannot be re-asked and a hot channel
 * like `terminal.data` must not spam the console.
 */
export async function listenDesktop<E extends DesktopEventName>(
  name: E,
  handler: (payload: DesktopEventPayload<E>) => void,
): Promise<UnlistenFn> {
  const schema = EVENT_SCHEMAS[name];
  return listen<unknown>(tauriEventName(name), (event) => {
    const parsed = schema.safeParse(event.payload);
    if (!parsed.success) {
      warnDroppedEvent(name, parsed.error);
      return;
    }
    handler(parsed.data as DesktopEventPayload<E>);
  });
}

const reportedDroppedEvents = new Set<string>();

function warnDroppedEvent(name: string, error: unknown): void {
  if (reportedDroppedEvents.has(name)) return;
  reportedDroppedEvents.add(name);
  console.warn(`[ipc] dropped a malformed "${name}" event payload`, error);
}

/**
 * A live native-event subscription.
 *
 * `stop()` is deliberately **synchronous**: every caller unsubscribes from a React
 * effect's cleanup or a teardown path, neither of which can await. `ready` exists
 * for the one caller that needs to know the subscription is actually established —
 * `useAgentRun` keeps its composer disabled until every agent channel is registered,
 * because sending a prompt into a not-yet-listening channel loses the answer.
 */
export type DesktopSubscription = {
  /** Resolves once the native side has accepted the channel; rejects if it did not. */
  ready: Promise<void>;
  /** Synchronous teardown. Safe to call before `ready` settles, and safe to call twice. */
  stop: () => void;
};

/**
 * Subscribe to a native event through the same gate as commands.
 *
 * This wraps the unlisten race that every call site had to solve by hand.
 * `listenDesktop` is async — Tauri hands back the unlisten function in a promise —
 * but teardown is synchronous, so the four existing call sites each wrote their own
 * version of:
 *
 *     let disposed = false;
 *     let unlisten;
 *     void listenDesktop(name, handler).then((stop) => {
 *       if (disposed) stop(); else unlisten = stop;
 *     });
 *     return () => { disposed = true; unlisten?.(); };
 *
 * The `disposed` flag is the whole point rather than bookkeeping. Without it, a
 * subscription that resolves *after* teardown has nobody left to call its unlisten,
 * so the native listener stays registered and keeps firing into a dead handler —
 * a leak visible only under fast navigation (open a terminal, switch server,
 * repeat), which is exactly the case review misses. `useAgentRun` and
 * `TerminalPane` each solved it with a flag, `ActivityFeed` with a flag plus an
 * `undefined` sentinel. Writing it once means it cannot be reintroduced by
 * reconstructing those four lines from memory.
 */
export function subscribeDesktop<E extends DesktopEventName>(
  name: E,
  handler: (payload: DesktopEventPayload<E>) => void,
): DesktopSubscription {
  let disposed = false;
  let unlisten: UnlistenFn | undefined;
  const ready = listenDesktop(name, handler).then((stop) => {
    // 订阅比清理晚到：当场退订，而不是存进一个再也不会被读到的变量。
    if (disposed) stop();
    else unlisten = stop;
  });
  return {
    ready,
    stop: () => {
      disposed = true;
      unlisten?.();
    },
  };
}

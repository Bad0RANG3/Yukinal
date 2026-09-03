/**
 * The only way the UI touches native capability (-R9-R10).
 *
 * Design constraints:
 * - `command` is keyed by the shared allow-list, so the UI cannot invent native surface.
 * - params/response types come from `IpcCommandMap`, the single contract both sides
 *   compile against. Rust mirrors the same names and shapes.
 * - When a command starts returning data that the UI branches on, a zod schema from
 *   `@yukinal/shared` is passed to `parse` — parse, never cast.
 */

import { invoke } from "@tauri-apps/api/core";
import type { IpcCommandMap, IpcCommandName } from "@yukinal/shared";

/** True when running inside Tauri; false in a plain browser during `vite dev`. */
export function isDesktopShell(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export class IpcUnavailableError extends Error {
  constructor(command: string) {
    super(`${command} is only available inside the Tauri shell (Rust core, )`);
    this.name = "IpcUnavailableError";
  }
}

export async function callDesktop<C extends IpcCommandName>(
  command: C,
  params: IpcCommandMap[C]["params"],
): Promise<IpcCommandMap[C]["response"]> {
  return callDesktopParsed(command, params, (raw) => raw as IpcCommandMap[C]["response"]);
}

/** Same as `callDesktop`, but validates/normalises the raw payload first. */
export async function callDesktopParsed<C extends IpcCommandName, T>(
  command: C,
  params: IpcCommandMap[C]["params"],
  parse: (raw: unknown) => T,
): Promise<T> {
  if (!isDesktopShell()) throw new IpcUnavailableError(command);
  const raw = await invoke<unknown>(command, { ...(params as Record<string, unknown>) });
  return parse(raw);
}

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
import { IPC_COMMANDS, IPC_SCHEMAS, type IpcCommandMap, type IpcCommandName } from "@yukinal/shared";

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

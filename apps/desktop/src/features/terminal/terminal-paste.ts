export const TERMINAL_INPUT_BYTE_LIMIT = 64 * 1024;

export interface TerminalPasteTarget {
  paste(text: string): void;
  focus(): void;
  readonly modes: { readonly bracketedPasteMode: boolean };
  readonly options: { readonly ignoreBracketedPasteMode?: boolean };
}

export class TerminalPasteError extends Error {}

/** xterm normalizes newlines and may add two six-byte bracketed-paste markers. */
export function validateTerminalPaste(text: string, bracketedPasteMode: boolean): void {
  const tooLarge = () => new TerminalPasteError("剪贴板文字超过终端单次 64 KiB 输入上限，请减少内容后重试。");
  if (text.length > TERMINAL_INPUT_BYTE_LIMIT) throw tooLarge();
  const encoder = new TextEncoder();
  if (encoder.encode(text).byteLength > TERMINAL_INPUT_BYTE_LIMIT) throw tooLarge();
  const inputBytes = encoder.encode(text.replace(/\r?\n/g, "\r")).byteLength + (bracketedPasteMode ? 12 : 0);
  if (inputBytes > TERMINAL_INPUT_BYTE_LIMIT) throw tooLarge();
  if (text.length === 0) throw new TerminalPasteError("剪贴板没有可粘贴的文字。");
}

/** Invoked only by a user gesture. A completed read never follows a new session. */
export async function pasteFromClipboard(
  terminal: TerminalPasteTarget,
  readText: () => Promise<string>,
  isCurrent: () => boolean,
): Promise<"pasted" | "stale"> {
  if (!isCurrent()) return "stale";
  let text: string;
  try {
    text = await readText();
  } catch {
    if (!isCurrent()) return "stale";
    throw new TerminalPasteError("无法读取剪贴板，请允许剪贴板访问后重试。");
  }
  if (!isCurrent()) return "stale";
  validateTerminalPaste(text, terminal.modes.bracketedPasteMode && terminal.options.ignoreBracketedPasteMode !== true);
  // Keep xterm's bracketed-paste handling; never append Enter or send a prefix.
  terminal.paste(text);
  terminal.focus();
  return "pasted";
}

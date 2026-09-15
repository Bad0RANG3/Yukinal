import { openUrl } from "@tauri-apps/plugin-opener";

import { isDesktopShell } from "./ipc.js";

/** URLs the Markdown renderer may hand to the operating system. */
export function isExternalOpenUrl(url: string): boolean {
  try {
    const parsed = new URL(url);
    return parsed.protocol === "http:" || parsed.protocol === "https:" || parsed.protocol === "mailto:";
  } catch {
    return false;
  }
}

/**
 * Open a link outside the WebView.
 *
 * The Tauri capability admits the same three schemes. In browser preview we use a
 * `noopener` tab instead, so preview cannot navigate the application window away.
 */
export async function openExternalUrl(url: string): Promise<void> {
  if (!isExternalOpenUrl(url)) throw new Error("Only http(s) and mailto links can be opened");
  if (isDesktopShell()) {
    await openUrl(url);
    return;
  }
  const opened = window.open(url, "_blank", "noopener,noreferrer");
  if (!opened) throw new Error("The browser blocked the external link");
}

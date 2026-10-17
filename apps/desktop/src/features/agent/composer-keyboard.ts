type ComposerKey = {
  key: string;
  shiftKey?: boolean;
  ctrlKey?: boolean;
  metaKey?: boolean;
  altKey?: boolean;
  isComposing?: boolean;
  keyCode?: number;
};

export function composerKeyAction(event: ComposerKey, suggestions: { open: boolean; canComplete: boolean }) {
  // IME confirmation belongs to the input method, even with suggestions open.
  if (event.isComposing || event.keyCode === 229) return "native";
  const plain = !event.shiftKey && !event.ctrlKey && !event.metaKey && !event.altKey;
  if (suggestions.open) {
    if (event.key === "ArrowDown") return "next";
    if (event.key === "ArrowUp") return "previous";
    if (event.key === "Escape") return "dismiss";
    if (plain && (event.key === "Enter" || event.key === "Tab") && suggestions.canComplete) return "complete";
  }
  if (event.key === "Enter" && (event.ctrlKey || event.metaKey) && !event.shiftKey && !event.altKey) return "submit";
  return "native";
}

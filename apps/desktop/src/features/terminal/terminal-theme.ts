import type { ITheme } from "@xterm/xterm";

const DARK_ANSI = {
  black: "#30343b", red: "#ef6b73", green: "#98c379", yellow: "#e5c07b",
  blue: "#61afef", magenta: "#c678dd", cyan: "#56b6c2", white: "#d7dae0",
  brightBlack: "#8b929e", brightRed: "#ff939b", brightGreen: "#b8e994", brightYellow: "#ffe39b",
  brightBlue: "#91caff", brightMagenta: "#e6a4f4", brightCyan: "#82dbe5", brightWhite: "#ffffff",
};
const LIGHT_ANSI = {
  black: "#202124", red: "#a31526", green: "#276323", yellow: "#765600",
  blue: "#174ea6", magenta: "#7b248a", cyan: "#00656d", white: "#555b66",
  brightBlack: "#626975", brightRed: "#bf1830", brightGreen: "#35722c", brightYellow: "#8a6100",
  brightBlue: "#245ec0", brightMagenta: "#9633a6", brightCyan: "#007d86", brightWhite: "#34383f",
};

/** Complete palettes, not a foreground/background override of xterm defaults. */
export function terminalTheme(mode: "light" | "dark", background?: string, foreground?: string): ITheme {
  const light = mode === "light";
  const bg = background || (light ? "#fafafa" : "#1d1d1d");
  const fg = foreground || (light ? "#202124" : "#f0f0f0");
  return {
    ...(light ? LIGHT_ANSI : DARK_ANSI), background: bg, foreground: fg,
    cursor: fg, cursorAccent: bg,
    selectionBackground: light ? "#c3d8f2" : "#465b78",
    selectionInactiveBackground: light ? "#dbe2ea" : "#394351",
    selectionForeground: light ? "#101820" : "#ffffff",
  };
}

export function readTerminalTheme(container: HTMLElement): ITheme {
  const style = getComputedStyle(container);
  const background = style.getPropertyValue("--bg").trim();
  const foreground = style.getPropertyValue("--text").trim();
  // Read resolved RGB as well as hex so a skin may use either CSS notation.
  const color = style.backgroundColor;
  const rgb = color.match(/^rgba?\(\s*([\d.]+)[, ]+\s*([\d.]+)[, ]+\s*([\d.]+)/);
  const explicit = container.closest('[data-theme="light"], [data-theme="dark"]')?.getAttribute("data-theme");
  const light = explicit ? explicit === "light" : rgb
    ? (Number(rgb[1]) * 0.2126 + Number(rgb[2]) * 0.7152 + Number(rgb[3]) * 0.0722) > 140
    : style.colorScheme === "light";
  return terminalTheme(light ? "light" : "dark", background, foreground);
}

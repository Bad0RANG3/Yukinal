/** A bounded, display-only format inside an explicitly labelled Markdown fence. */
export type ConversationWidget =
  | { type: "card"; title: string; body: string; tone: "neutral" | "info" | "success" | "warning" | "error" }
  | { type: "steps"; title: string; items: { title: string; detail?: string; status: "pending" | "active" | "done" | "error" }[] }
  | { type: "metrics"; title: string; items: { label: string; value: string | number; unit?: string; trend?: number[] }[] }
  | { type: "progress"; title: string; value: number; max: number; detail?: string }
  | { type: "details"; title: string; body: string };

export interface ConversationWidgetDocument { version: 1; widgets: ConversationWidget[] }
export type ConversationWidgetParse =
  | { status: "ready"; document: ConversationWidgetDocument }
  | { status: "invalid"; reason: "json" | "shape" | "size" };

export const CONVERSATION_WIDGET_LANGUAGE = "yukinal-ui";
export const MAX_CONVERSATION_WIDGET_SOURCE = 65_536;
const TONES = ["neutral", "info", "success", "warning", "error"] as const;
const STEP_STATES = ["pending", "active", "done", "error"] as const;
type RecordValue = Record<string, unknown>;
const object = (value: unknown): value is RecordValue => value !== null && typeof value === "object" && !Array.isArray(value);
const keys = (value: RecordValue, allowed: readonly string[]) => Object.keys(value).every((key) => allowed.includes(key));
const text = (value: unknown, max = 160): value is string => typeof value === "string" && value.length <= max;
const title = (value: unknown): value is string => text(value) && value.trim().length > 0;
const finite = (value: unknown): value is number => typeof value === "number" && Number.isFinite(value);
const optionalText = (value: unknown, max = 8_000) => value === undefined || text(value, max);
const array = (value: unknown, max: number): value is unknown[] => Array.isArray(value) && value.length > 0 && value.length <= max;

function widget(value: unknown): ConversationWidget | null {
  if (!object(value) || !title(value.title)) return null;
  switch (value.type) {
    case "card": {
      if (!keys(value, ["type", "title", "body", "tone"]) || !text(value.body, 8_000)) return null;
      const tone = value.tone === undefined ? "neutral" : TONES.find((item) => item === value.tone);
      return tone ? { type: "card", title: value.title, body: value.body, tone } : null;
    }
    case "details":
      return keys(value, ["type", "title", "body"]) && text(value.body, 8_000)
        ? { type: "details", title: value.title, body: value.body } : null;
    case "progress":
      return keys(value, ["type", "title", "value", "max", "detail"]) && finite(value.value) && finite(value.max) && value.max > 0 && value.value >= 0 && value.value <= value.max && optionalText(value.detail)
        ? { type: "progress", title: value.title, value: value.value, max: value.max, ...(value.detail !== undefined ? { detail: value.detail as string } : {}) } : null;
    case "steps": {
      if (!keys(value, ["type", "title", "items"]) || !array(value.items, 20)) return null;
      const items: Extract<ConversationWidget, { type: "steps" }>["items"] = [];
      for (const item of value.items) {
        if (!object(item) || !keys(item, ["title", "status", "detail"]) || !title(item.title) || !optionalText(item.detail)) return null;
        const status = STEP_STATES.find((state) => state === item.status);
        if (!status) return null;
        items.push({ title: item.title, status, ...(item.detail !== undefined ? { detail: item.detail as string } : {}) });
      }
      return { type: "steps", title: value.title, items };
    }
    case "metrics": {
      if (!keys(value, ["type", "title", "items"]) || !array(value.items, 8)) return null;
      const items: Extract<ConversationWidget, { type: "metrics" }>["items"] = [];
      for (const item of value.items) {
        if (!object(item) || !keys(item, ["label", "value", "unit", "trend"]) || !title(item.label) || !(text(item.value, 120) || finite(item.value)) || !optionalText(item.unit, 32)) return null;
        if (item.trend !== undefined && (!Array.isArray(item.trend) || item.trend.length > 40 || !item.trend.every(finite))) return null;
        items.push({ label: item.label, value: item.value, ...(item.unit !== undefined ? { unit: item.unit as string } : {}), ...(item.trend !== undefined ? { trend: [...item.trend as number[]] } : {}) });
      }
      return { type: "metrics", title: value.title, items };
    }
    default: return null;
  }
}

/** No actions, URLs, HTML, remote assets or permissive coercion enter the renderer. */
export function parseConversationWidgets(source: string): ConversationWidgetParse {
  if (source.length > MAX_CONVERSATION_WIDGET_SOURCE) return { status: "invalid", reason: "size" };
  let value: unknown;
  try { value = JSON.parse(source); } catch { return { status: "invalid", reason: "json" }; }
  if (!object(value) || !keys(value, ["version", "widgets"]) || value.version !== 1 || !array(value.widgets, 12)) return { status: "invalid", reason: "shape" };
  const widgets: ConversationWidget[] = [];
  for (const item of value.widgets) {
    const parsed = widget(item);
    if (!parsed) return { status: "invalid", reason: "shape" };
    widgets.push(parsed);
  }
  return { status: "ready", document: { version: 1, widgets } };
}

/** Completed items can appear while the following JSON item is still arriving.
 * Only the documented envelope prefix is accepted; every finished item uses the same validator.
 */
export function parseStreamingConversationWidgets(source: string): ConversationWidgetDocument | null {
  if (source.length > MAX_CONVERSATION_WIDGET_SOURCE) return null;
  const prefix = /^\s*\{\s*"version"\s*:\s*1\s*,\s*"widgets"\s*:\s*\[/.exec(source);
  if (!prefix) return null;
  let cursor = prefix[0].length;
  let ready: ConversationWidgetDocument | null = null;
  const items: string[] = [];
  while (cursor < source.length) {
    while (/\s/.test(source[cursor] ?? "") && cursor < source.length) cursor++;
    if (cursor === source.length || source[cursor] === "]") return ready;
    if (source[cursor] !== "{") return null;
    const start = cursor;
    let depth = 0, quoted = false, escaped = false, complete = false;
    for (; cursor < source.length; cursor++) {
      const char = source[cursor];
      if (quoted) {
        if (escaped) escaped = false;
        else if (char === "\\") escaped = true;
        else if (char === '"') quoted = false;
      } else if (char === '"') quoted = true;
      else if (char === "{" || char === "[") depth++;
      else if (char === "}" || char === "]") {
        depth--;
        if (depth === 0) { cursor++; complete = true; break; }
      }
    }
    if (!complete) return ready;
    items.push(source.slice(start, cursor));
    const parsed = parseConversationWidgets('{"version":1,"widgets":[' + items.join(",") + "]}");
    if (parsed.status !== "ready") return null;
    ready = parsed.document;
    while (/\s/.test(source[cursor] ?? "") && cursor < source.length) cursor++;
    if (cursor === source.length || source[cursor] === "]") return ready;
    if (source[cursor] !== ",") return null;
    cursor++;
  }
  return ready;
}

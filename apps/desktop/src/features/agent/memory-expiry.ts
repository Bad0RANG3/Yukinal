export type ExpiryPreset = "never" | "7d" | "30d" | "90d" | "custom";

const DAY_MS = 24 * 60 * 60 * 1000;

export function toDatetimeLocalValue(iso: string | undefined): string {
  if (!iso) return "";
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "";
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

export function resolveExpiryIso(preset: ExpiryPreset, customLocal: string, nowMs = Date.now()): string | undefined {
  if (preset === "never") return undefined;
  if (preset === "7d") return new Date(nowMs + 7 * DAY_MS).toISOString();
  if (preset === "30d") return new Date(nowMs + 30 * DAY_MS).toISOString();
  if (preset === "90d") return new Date(nowMs + 90 * DAY_MS).toISOString();
  const trimmed = customLocal.trim();
  if (!trimmed) return undefined;
  const parsed = new Date(trimmed);
  if (Number.isNaN(parsed.getTime())) throw new Error("请输入有效的过期时间。");
  return parsed.toISOString();
}

export function formatMemoryExpiry(iso: string | undefined): string | null {
  if (!iso) return null;
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return date.toLocaleString("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
}

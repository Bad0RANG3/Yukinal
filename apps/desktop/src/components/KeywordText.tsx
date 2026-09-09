import type { ReactNode } from "react";

const TOKEN_PATTERN = /https?:\/\/[^\s"'<>]+|(?:[A-Za-z]:\\|\/)[^\s"'<>]+|[A-Za-z_][A-Za-z0-9_.-]*(?=\s*=)|\b[A-Za-z][A-Za-z0-9_-]*\.[A-Za-z][A-Za-z0-9_.-]*\b|\b(?:error|errors|failed|failure|fatal|panic|exception|denied|timeout|timed out|warn|warning|degraded|retry|retries|success|successful|healthy|running|connected|active|ok|pending|waiting|connecting|queued|stopped|disconnected)\b/gi;

type KeywordKind = "error" | "warning" | "success" | "pending" | "identifier" | "path" | "key";

export function KeywordText({ text, className }: { text: string; className?: string }) {
  const nodes: ReactNode[] = [];
  let cursor = 0;

  for (const match of text.matchAll(TOKEN_PATTERN)) {
    const value = match[0];
    const start = match.index ?? 0;
    if (start > cursor) nodes.push(text.slice(cursor, start));
    nodes.push(
      <span className={`keyword-token keyword-token-${classify(value)}`} key={`${start}:${value}`}>
        {value}
      </span>,
    );
    cursor = start + value.length;
  }

  if (cursor < text.length) nodes.push(text.slice(cursor));
  return <span className={className}>{nodes.length ? nodes : text}</span>;
}

function classify(value: string): KeywordKind {
  if (/^https?:\/\//i.test(value) || /^(?:[A-Za-z]:\\|\/)/.test(value)) return "path";
  if (/^[A-Za-z_][A-Za-z0-9_.-]*$/.test(value) && /[A-Z_]/.test(value)) return "key";
  if (/^[A-Za-z][A-Za-z0-9_-]*\.[A-Za-z][A-Za-z0-9_.-]*$/.test(value)) return "identifier";

  const normalized = value.toLowerCase();
  if (/error|fail|fatal|panic|exception|denied|timeout/.test(normalized)) return "error";
  if (/warn|degrad|retry/.test(normalized)) return "warning";
  if (/success|healthy|running|connected|active|ok/.test(normalized)) return "success";
  return "pending";
}

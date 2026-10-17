import { z } from "zod";
import type { ToolExecutionRecord } from "@yukinal/shared";

export const WORKPAGE_TOOLS = ["files", "terminal", "logs", "services"] as const;
export type WorkpageTool = (typeof WORKPAGE_TOOLS)[number];
export type WorkpageDock = "left" | "right";
export const WORKPAGE_LABEL = { files: "文件", terminal: "终端", logs: "日志", services: "服务", evidence: "证据", artifact: "产物", board: "看板" } as const;

const Text = z.string().max(200_000);
const ArtifactContentSchema = z.discriminatedUnion("format", [
  z.object({ format: z.literal("markdown"), text: Text }),
  z.object({ format: z.literal("html"), html: Text }),
  z.object({ format: z.literal("chart"), series: z.array(z.object({ label: z.string().max(160), value: z.number().finite() })).min(1).max(80), unit: z.string().max(40).optional() }),
  z.object({ format: z.literal("flow"), nodes: z.array(z.object({ id: z.string().min(1).max(100), label: z.string().max(300) })).min(1).max(40), edges: z.array(z.object({ from: z.string().max(100), to: z.string().max(100), label: z.string().max(100).optional() })).max(80) }),
  z.object({ format: z.literal("json"), text: Text }),
]);
export type ArtifactContent = z.output<typeof ArtifactContentSchema>;
export type WorkpageArtifact = { id: string; title: string; content: ArtifactContent; serverId?: string | null; createdAt?: string };
export type WorkpageEvidence = { id: string; title: string; actor: string; target: string; execution: ToolExecutionRecord };

type PageBase = { id: string; title: string; serverId: string | null; dock: WorkpageDock };
export type Workpage = PageBase & (
  | { kind: WorkpageTool }
  | { kind: "board" }
  | { kind: "evidence"; evidence: WorkpageEvidence }
  | { kind: "artifact"; artifact: WorkpageArtifact }
);

/** Only explicit, bounded envelopes become visuals. Unknown output stays text. */
export function artifactFromContent(id: string, title: string, content: unknown, serverId?: string | null): WorkpageArtifact {
  const parsed = ArtifactContentSchema.safeParse(content);
  let body: ArtifactContent;
  if (parsed.success) {
    body = parsed.data;
    if (body.format === "flow") {
      const ids = new Set(body.nodes.map((node) => node.id));
      if (ids.size !== body.nodes.length || body.edges.some((edge) => !ids.has(edge.from) || !ids.has(edge.to))) {
        body = { format: "json", text: printableContent(content) };
      }
    }
  } else {
    body = typeof content === "string" ? { format: "markdown", text: content.slice(0, 200_000) } : { format: "json", text: printableContent(content) };
  }
  return { id, title, content: body, serverId };
}

export function printableContent(value: unknown): string {
  if (typeof value === "string") return value.slice(0, 200_000);
  try { return (JSON.stringify(value, null, 2) ?? "无内容").slice(0, 200_000); }
  catch { return "无法显示内容"; }
}

/** Opaque-origin browser iframe. CSP restricts resource requests; native scripts stay disabled. */
export function sandboxedArtifactDocument(html: string): string {
  const policy = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; font-src 'none'; connect-src 'none'; frame-src 'none'; child-src 'none'; worker-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'";
  return `<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="${policy}"><meta name="referrer" content="no-referrer"><style>html{color-scheme:dark}body{margin:16px;font:14px/1.5 system-ui;background:#121212;color:#e8e8e8}button,input,select{font:inherit}*{box-sizing:border-box}</style></head><body>${html}</body></html>`;
}

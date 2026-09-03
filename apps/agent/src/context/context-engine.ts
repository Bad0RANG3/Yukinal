/**
 * Context Engine — layered context, not a data firehose.
 *
 * MVP rule: assemble only the layers the current task needs, and never reach for a
 * vector store before there is a corpus to search.
 */

import type { AgentRunRequest, Server, ServerSnapshot, Workspace } from "@yukinal/shared";

export const CONTEXT_LAYERS = [
  "global",
  "workspace",
  "server",
  "task",
  "toolResult",
  "conversation",
] as const;

export type ContextLayerName = (typeof CONTEXT_LAYERS)[number];

/** Injected so the runtime never imports Rust or SQLite directly. */
export interface ContextSource {
  server(serverId: string): Promise<Server | undefined>;
  snapshot(serverId: string): Promise<ServerSnapshot | undefined>;
  workspace(workspaceId: string): Promise<Workspace | undefined>;
}

export interface ServerContext {
  server: { id: string; name: string; os: string; hostname?: string; environment: Server["metadata"]["environment"] };
  metrics: { cpu?: number; memory?: number; disk?: number };
  runtime: { docker: boolean };
  containers: Array<{ name: string; status: string; state: string }>;
  health: ServerSnapshot["health"];
}

export interface ContextBundle {
  runId: string;
  layers: ContextLayerName[];
  server?: ServerContext;
  workspace?: Pick<Workspace, "id" | "name" | "defaultEnvironment">;
  /** Text handed to the model as the system/context block (shape). */
  rendered: string;
  /** Truncation must be visible, not silent. */
  truncated: boolean;
}

export interface ContextEngineOptions {
  /** Hard cap on the rendered block; token accounting lands together with the provider. */
  maxRenderedChars?: number;
}

export class ContextEngine {
  readonly #maxRenderedChars: number;

  constructor(
    private readonly source: ContextSource,
    options: ContextEngineOptions = {},
  ) {
    this.#maxRenderedChars = options.maxRenderedChars ?? 12_000;
  }

  async build(request: AgentRunRequest): Promise<ContextBundle> {
    const layers: ContextLayerName[] = ["global", "task"];
    const serverId = request.target?.serverId ?? request.focusServerId;

    let serverContext: ServerContext | undefined;
    let workspaceContext: ContextBundle["workspace"];

    if (request.workspaceId) {
      const workspace = await this.source.workspace(request.workspaceId);
      if (workspace) {
        layers.push("workspace");
        workspaceContext = {
          id: workspace.id,
          name: workspace.name,
          defaultEnvironment: workspace.defaultEnvironment,
        };
      }
    }

    if (serverId) {
      const server = await this.source.server(serverId);
      if (server) {
        layers.push("server");
        const snapshot = await this.source.snapshot(serverId);
        serverContext = toServerContext(server, snapshot);
      }
    }

    const rendered = render({ workspace: workspaceContext, server: serverContext, request });
    const truncated = rendered.length > this.#maxRenderedChars;

    return {
      runId: request.runId,
      layers,
      server: serverContext,
      workspace: workspaceContext,
      rendered: truncated ? rendered.slice(0, this.#maxRenderedChars) : rendered,
      truncated,
    };
  }
}

function toServerContext(server: Server, snapshot: ServerSnapshot | undefined): ServerContext {
  const disks = snapshot?.disks ?? [];
  const worstDisk = disks.reduce<number>((max, disk) => Math.max(max, disk.usagePercent), 0);

  return {
    server: {
      id: server.id,
      name: server.name,
      os: snapshot?.os ? `${snapshot.os.distribution} ${snapshot.os.version}` : (server.metadata.os ?? "unknown"),
      hostname: server.metadata.hostname,
      environment: server.metadata.environment,
    },
    metrics: {
      cpu: round(snapshot?.cpu?.usagePercent),
      memory: round(snapshot?.memory?.usagePercent),
      disk: round(worstDisk || undefined),
    },
    runtime: { docker: server.capabilities.docker === true },
    containers: (snapshot?.docker?.containers ?? []).map((container) => ({
      name: container.name,
      status: container.status,
      state: container.state,
    })),
    health: snapshot?.health ?? "unknown",
  };
}

function round(value: number | undefined): number | undefined {
  return value === undefined ? undefined : Math.round(value);
}

function render(parts: {
  workspace?: ContextBundle["workspace"];
  server?: ServerContext;
  request: AgentRunRequest;
}): string {
  const lines: string[] = [];
  if (parts.workspace) {
    lines.push(`Workspace: ${parts.workspace.name} (default environment: ${parts.workspace.defaultEnvironment})`);
  }
  if (parts.server) {
    lines.push(`Focused server: ${parts.server.server.name} [${parts.server.server.id}] (${parts.server.server.environment})`);
    lines.push(`Runtime: ${JSON.stringify({ os: parts.server.server.os, health: parts.server.health, metrics: parts.server.metrics, containers: parts.server.containers })}`);
  } else {
    lines.push("Focused server: none — resolve the target before any write action.");
  }
  lines.push(`Task: ${parts.request.prompt}`);
  return lines.join("\n");
}

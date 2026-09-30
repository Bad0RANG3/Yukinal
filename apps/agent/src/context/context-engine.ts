/**
 * Context Engine — layered context, not a data firehose.
 *
 * MVP rule: assemble only the layers the current task needs, and never reach for a
 * vector store before there is a corpus to search.
 */

import type {
  AgentRunRequest,
  InvestigationContext,
  Server,
  ServerSnapshot,
  Workspace,
} from "@yukinal/shared";

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
  investigation(taskId: string): Promise<InvestigationContext | undefined>;
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
  /** Host-read task and plan context; never supplied by the model. */
  investigation?: InvestigationContext;
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
    const investigation = request.taskId ? await this.source.investigation(request.taskId) : undefined;

    // Workspace and server identity are independent lookups. Fetch them in
    // parallel; the snapshot still waits for the validated server id below.
    const [workspace, server] = await Promise.all([
      request.workspaceId ? this.source.workspace(request.workspaceId) : Promise.resolve(undefined),
      serverId ? this.source.server(serverId) : Promise.resolve(undefined),
    ]);

    if (workspace) {
      layers.push("workspace");
      workspaceContext = {
        id: workspace.id,
        name: workspace.name,
        defaultEnvironment: workspace.defaultEnvironment,
      };
    }

    if (server && serverId) {
      layers.push("server");
      const snapshot = await this.source.snapshot(serverId);
      serverContext = toServerContext(server, snapshot);
    }

    const rendered = render({ workspace: workspaceContext, server: serverContext, investigation, request });
    const truncated = rendered.length > this.#maxRenderedChars;

    return {
      runId: request.runId,
      layers,
      server: serverContext,
      workspace: workspaceContext,
      investigation,
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
  investigation?: InvestigationContext;
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
    lines.push("Focused server: none — answer general questions directly; ask for a server before remote actions.");
  }
  if (parts.investigation) {
    const { task, evidence, findings, runs, steps, decisionBrief, plan, artifacts } = parts.investigation;
    lines.push(`Investigation: ${task.objective} [${task.status}/${task.phase}]`);
    lines.push(`Investigation budget: ${task.budget.maxSteps} steps, ${task.budget.maxRunMs}ms, ${task.budget.maxAttempts} attempts`);
    if (task.lastFailure) {
      lines.push(`Previous failure (${task.lastFailure.code}, retryable=${task.lastFailure.retryable}): ${task.lastFailure.message}`);
      if (task.lastFailure.options?.length) {
        lines.push(`Failure options: ${task.lastFailure.options.map((option) => `${option.id}=${option.title}`).join("; ")}`);
      }
      if (rollbackWasRequested(task.lastFailure.detail)) {
        lines.push("Rollback request: the user selected a separately gated inverse plan; do not replay the old action or rollback text, and require a fresh baseline, scope check, exact bindings, and approval.");
      }
      if (freshBaselineRequired(task.lastFailure.detail)) {
        lines.push("Recovery safety: the previous approval, change baseline and running observation window are invalid after interruption; re-check target identity, collect fresh read-only evidence, and obtain a new plan before replaying an action.");
      }
    }
    lines.push(`Evidence collected: ${evidence.length}; findings: ${findings.length}; artifacts: ${artifacts.length}`);
    const commandGrant = task.guardrails.commandGrant;
    if (commandGrant) {
      lines.push(
        `User-delegated server.exec scope: ${commandGrant.serverId} (${commandGrant.environment}), remaining calls=${Math.max(0, commandGrant.maxCalls - commandGrant.callsUsed)}, duration=${Math.max(0, commandGrant.maxTotalDurationMs - commandGrant.totalDurationMs)}ms, output=${Math.max(0, commandGrant.maxTotalOutputBytes - commandGrant.totalOutputBytes)} bytes, expires=${commandGrant.expiresAt}; known critical patterns still require direct approval.`,
      );
    }
    const freshnessCounts = evidence.reduce<Record<string, number>>((counts, item) => {
      const status = item.freshness?.status ?? "unknown";
      counts[status] = (counts[status] ?? 0) + 1;
      return counts;
    }, {});
    const freshnessSummary = Object.entries(freshnessCounts)
      .map(([status, count]) => `${status}=${count}`)
      .join(", ");
    lines.push(`Evidence freshness (host policy): ${freshnessSummary || "unknown=0"}`);
    if ((freshnessCounts.expired ?? 0) > 0 || (freshnessCounts.stale ?? 0) > 0) {
      lines.push("Freshness warning: stale/expired evidence is historical context only; collect a fresh read-only sample before treating current state as proven or proposing a change.");
    }
    lines.push(`Runs: ${runs.length}; steps: ${steps.length}`);
    if (plan) {
      lines.push(`Active plan revision ${plan.revision} [${plan.status}]${plan.currentStepId ? ` current=${plan.currentStepId}` : ""}; approval=${plan.approval?.status ?? "pending"}${plan.approval?.optionId ? `(${plan.approval.optionId})` : ""}`);
      if (plan.observationWindow) {
        const window = plan.observationWindow;
        lines.push(
          `Observation window [${window.status}] samples=${window.sampleCount}; duration=${window.durationSeconds}s; interval=${window.intervalSeconds}s; tools=${window.allowedTools.join(", ")}${window.deadlineAt ? `; deadline=${window.deadlineAt}` : ""}`,
        );
        lines.push(`Observation criteria: ${window.successCriteria.join("; ")}`);
        if (window.lastFailure) lines.push(`Observation failure: ${window.lastFailure}`);
      }
      for (const item of plan.steps.slice(0, 16)) {
        lines.push(
          `Plan step ${item.ordinal} ${item.title}: ${item.status}; kind=${item.kind}; tools=${item.allowedTools.join(", ")}; attempts=${item.attempts}/${item.maxAttempts}; approval=${item.requiresApproval}; idempotency=${item.idempotency ?? "unspecified"}; baseline=${item.requiresBaseline === true ? "required" : "not-required"}`,
        );
        if (item.riskLevel) lines.push(`Plan step risk: ${item.riskLevel}`);
        if (item.preconditions?.length) lines.push(`Plan preconditions: ${item.preconditions.join("; ")}`);
        if (item.preview) lines.push(`Plan preview: ${item.preview}`);
        if (item.verificationCriteria?.length) lines.push(`Plan verification: ${item.verificationCriteria.join("; ")}`);
        if (item.rollback) lines.push(`Plan rollback: ${item.rollback}`);
        if (item.evidenceIds.length > 0) lines.push(`Plan evidence prerequisites: ${item.evidenceIds.join(", ")}`);
        if (item.successCriteria.length > 0) lines.push(`Plan success criteria: ${item.successCriteria.join("; ")}`);
      }
    } else {
      lines.push("No active plan: call investigation.plan before using task tools.");
    }
    for (const run of runs.slice(0, 4)) {
      lines.push(`Run attempt ${run.attempt}: ${run.status}${run.failure ? ` (${run.failure.code})` : ""}`);
    }
    for (const step of steps.slice(-8)) {
      lines.push(`Step ${step.ordinal} ${step.title}: ${step.status}${step.planStepId ? ` [plan=${step.planId ?? "?"}/${step.planStepId}]` : ""}${step.outputSummary ? ` — ${step.outputSummary}` : ""}`);
    }
    for (const item of findings.slice(0, 16)) {
      lines.push(`Finding (${item.confidence}): ${item.title} — ${item.statement}`);
    }
    for (const item of evidence.slice(0, 16)) {
      const freshness = item.freshness;
      const freshnessDetail = freshness
        ? `freshness=${freshness.status}; age=${freshness.ageSeconds ?? "unknown"}s; policy=${freshness.policy}`
        : "freshness=unknown";
      lines.push(`Evidence ${item.sourceTool} (${item.collectedAt}; ${freshnessDetail}): ${item.inputSummary}`);
    }
    for (const artifact of artifacts.slice(0, 12)) {
      lines.push(`Artifact ${artifact.kind} [${artifact.status}] ${artifact.title}: ${artifact.summary}`);
    }
    if (decisionBrief) {
      lines.push(
        `Decision brief [${decisionBrief.status}]${decisionBrief.selectedOptionId ? ` selected=${decisionBrief.selectedOptionId}` : ""}`,
      );
      lines.push(
        `Decision options: ${decisionBrief.options
          .map((option) => `${option.title} [${option.status}, ${option.riskLevel}]`)
          .join("; ") || "none"}`,
      );
    }
  }
  lines.push(`Task: ${parts.request.prompt}`);
  return lines.join("\n");
}

function rollbackWasRequested(detail: unknown): boolean {
  if (!detail || typeof detail !== "object" || Array.isArray(detail)) return false;
  return (detail as Record<string, unknown>).rollbackRequested === true;
}

function freshBaselineRequired(detail: unknown): boolean {
  if (!detail || typeof detail !== "object" || Array.isArray(detail)) return false;
  return (detail as Record<string, unknown>).requiresFreshBaseline === true;
}

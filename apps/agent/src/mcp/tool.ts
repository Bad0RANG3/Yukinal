/**
 * The MCP adapter: a catalog entry becomes a `Tool` (ADR 0014).
 *
 * This file is the *only* place where an MCP tool becomes a Yukinal tool. Everything after
 * this point is the ordinary path — permission engine, ticket, registry timeout, trace,
 * audit — and nothing here may shorten it. Three rules decide how it is written:
 *
 * 1. **The remote server does not get to say how dangerous it is.** The host owns the
 *    effective risk and sends it as `risk` (ADR 0074). By default the host does not trust
 *    the server's annotations and maps every MCP tool to `critical`; a user may explicitly
 *    trust one server, and then the host maps that server's bounded hints to `low`/`medium`/
 *    `high`. This adapter only reads the host's answer — it never looks at `annotations`
 *    itself — and treats a missing or unrecognised value as `critical`.
 * 2. **The remote server does not get to define the input contract.** The local Zod schema is
 *    a record of unknown values; the server's `inputSchema` is appended to the *description*
 *    as untrusted documentation the model can read. Validating model input against a document
 *    the model (or the server) can influence would be a validation hole with extra steps.
 * 3. **The host executes, not this process.** `host.tool.execute` runs the call; a tool here
 *    never spawns anything (ADR 0001, ADR 0008).
 */

import { z } from "zod";

import {
  isValidInternalToolName,
  type HostMcpCatalogTool,
  type HostMcpToolCallOutput,
  type HostToolExecuteResponse,
  type RiskLevel,
  type ToolOrigin,
  type ToolTarget,
} from "@yukinal/shared";

import { ToolFailure, type Tool } from "../tools/tool.js";

/**
 * The value used when the host does not send a risk, or sends one this build does not know.
 *
 * Not a placeholder: it is the honest value for "a third-party process whose side effects
 * this repo has no way to bound", and the value the host itself sends for an untrusted
 * server (ADR 0074). See the module doc, rule 1.
 */
export const MCP_TOOL_RISK = "critical" as const;

const MCP_TOOL_RISKS: readonly RiskLevel[] = ["low", "medium", "high", "critical"];

/**
 * The host's effective risk, bounded to the four tiers this build understands.
 *
 * A missing field (older host) or an unknown value fails closed to `critical`. The string
 * comparison is deliberate: the field crosses a JSON boundary, so it is `string` until it is
 * checked here.
 */
export function resolveMcpRisk(risk: string | undefined): RiskLevel {
  return MCP_TOOL_RISKS.find((candidate) => candidate === risk) ?? MCP_TOOL_RISK;
}

/**
 * MCP calls are allowed to run longer than built-in ones (the host's own per-request ceiling
 * is 30s, `crates/core/src/mcp/mod.rs`) so that the *host* is the one that gives up first.
 * If the registry's timer fired first the agent would report a timeout while the server kept
 * running, which is exactly the kind of lie the rest of this file is written to avoid.
 */
export const MCP_TOOL_TIMEOUT_MS = 45_000;

/**
 * One attempt. A retried MCP call can re-run a side effect: the host already says
 * `retryable: false` for everything except its own timeouts, and a timeout is precisely the
 * case where the server may have done the work and lost the answer.
 */
export const MCP_TOOL_RETRY = { maxAttempts: 1, backoffMs: 0 } as const;

/** The description shown when a server declares none (MCP allows an empty description). */
const FALLBACK_DESCRIPTION = "This tool is provided by an MCP server and describes itself as having no description.";

/** How much of the remote schema is appended to the description. The registry's own output
 * summary cap is 4000 characters; the same budget keeps a hostile server from pushing the
 * rest of the prompt out of the context window. */
const MAX_REMOTE_SCHEMA_CHARS = 2_000;
const MAX_REMOTE_DESCRIPTION_CHARS = 4_000;

/**
 * The local input contract for every MCP tool: "an object, and the server validates it".
 *
 * Deliberately loose. `tools/call` takes `arguments`, which every MCP tool defines for
 * itself; the host passes the object through untouched and the server is the authority on
 * whether it is right. What this schema *does* guarantee is that the model cannot send
 * something that is not an object at all — `invalid_input` from the registry, before a
 * subprocess is involved.
 */
export const MCP_TOOL_INPUT = z.record(z.string(), z.unknown());

export interface McpToolOptions {
  /** The host request executor (`HostRpcClient.execute`). */
  executeOnHost(request: {
    callId: string;
    traceId: string;
    toolName: string;
    input: unknown;
    target: ToolTarget;
    taskId?: string;
    planId?: string;
    planStepId?: string;
    evidenceIds?: string[];
  }, signal?: AbortSignal): Promise<HostToolExecuteResponse>;
}

/** A failure message that says which server, which tool, and what to do next. */
function describe(serverId: string, tool: string, message: string): string {
  return `MCP server "${serverId}" tool "${tool}": ${message}`;
}

/**
 * Build the model-facing description: the server's own words (untrusted, bounded) plus the
 * remote input schema as documentation.
 *
 * The result is still untrusted text — it is a description, and the trace UI and prompt
 * builder are responsible for how they present it (the repository `docs/boundaries/mcp.md`,
 * 「边界：外部工具（MCP）」: description text is untrusted data).
 */
export function describeMcpTool(tool: HostMcpCatalogTool): string {
  const remote = tool.description.trim().slice(0, MAX_REMOTE_DESCRIPTION_CHARS);
  const header = tool.remoteName === undefined ? undefined : `Called "${tool.remoteName}" on the server.`;
  const body = remote.length > 0 ? remote : FALLBACK_DESCRIPTION;
  let schema: string;
  try {
    schema = JSON.stringify(tool.inputSchema);
  } catch {
    schema = "[unserialisable]";
  }
  const trimmed = schema.length > MAX_REMOTE_SCHEMA_CHARS ? `${schema.slice(0, MAX_REMOTE_SCHEMA_CHARS)}…` : schema;
  return [
    header,
    body,
    `Provided by MCP server "${tool.serverId}". Its arguments are validated by that server, so the`,
    "schema below is documentation, not a contract this agent enforces:",
    trimmed,
  ]
    .filter((part): part is string => part !== undefined)
    .join("\n");
}

/**
 * One catalog entry → one `Tool`.
 *
 * Throws (rather than returning `undefined`) for a name that is not a valid internal name:
 * that means the host handed over something it should not have, and swallowing it would hide
 * a real disagreement between the two sides of the contract. [`registerCatalog`] decides what
 * a bad entry costs (that one tool, not the whole catalog).
 */
export function mcpToolFromCatalog(tool: HostMcpCatalogTool, options: McpToolOptions): Tool {
  if (!isValidInternalToolName(tool.name)) {
    throw new Error(
      `MCP tool name "${tool.name}" is not a valid internal tool name (ADR 0004); the host must map it before it reaches the agent.`,
    );
  }
  if (tool.serverId.trim().length === 0) {
    throw new Error(`MCP tool "${tool.name}" has no serverId, so a call to it could not be attributed (ADR 0014).`);
  }
  const origin: ToolOrigin = { kind: "mcp", serverId: tool.serverId };
  const description = describeMcpTool(tool);
  const risk = resolveMcpRisk(tool.risk);

  return {
    name: tool.name,
    description,
    risk,
    timeoutMs: MCP_TOOL_TIMEOUT_MS,
    cancellable: true,
    retry: { ...MCP_TOOL_RETRY },
    // Only a host-resolved `low` tool is treated as non-effectful. Everything else keeps the
    // durable-plan and ledger requirements (ADR 0074).
    effectful: risk !== "low",
    origin,
    input: MCP_TOOL_INPUT,
    async execute(input, context) {
      const response = await options.executeOnHost(
        {
          callId: context.callId,
          traceId: context.traceId,
          toolName: tool.name,
          input,
          target: context.target,
          ...(context.taskId ? { taskId: context.taskId } : {}),
          ...(context.planId ? { planId: context.planId } : {}),
          ...(context.planStepId ? { planStepId: context.planStepId } : {}),
          ...(context.evidenceIds ? { evidenceIds: context.evidenceIds } : {}),
        },
        context.signal,
      );

      if (response.status === "cancelled") {
        // The registry turns this into a `cancelled` result; throwing with the same code keeps
        // the reason (user Stop vs the host's own cancellation) in one place.
        throw new ToolFailure(
          response.error?.message ?? describe(tool.serverId, tool.tool, "the call was cancelled"),
          "cancelled",
          false,
          response.error?.detail,
        );
      }
      if (response.status === "failed") {
        throw new ToolFailure(
          describe(tool.serverId, tool.tool, response.error.message),
          response.error.code,
          // The host already decided; a retry here would only re-send the same call.
          response.error.retryable,
          response.error.detail,
        );
      }

      const output = asCallOutput(response.output);
      if (output === undefined) {
        throw new ToolFailure(
          describe(tool.serverId, tool.tool, "the host returned a result this agent cannot read"),
          "internal",
          false,
          response.output,
        );
      }
      if (output.isError) {
        // "The tool ran and reported an error" is not "the call failed". It comes back as a
        // ToolFailure so the model gets the same vocabulary for both, but with the server's
        // own words — a tool-level error is usually actionable (wrong arguments, and so on).
        throw new ToolFailure(
          describe(tool.serverId, output.tool, output.text.trim() || "the tool reported an error"),
          "execution_failed",
          false,
          { ...(output.structuredContent === undefined ? {} : { structuredContent: output.structuredContent }) },
        );
      }
      return output;
    },
  };
}

/**
 * Read a `host.tool.execute` MCP output.
 *
 * The shape is checked here rather than trusted: this is a process boundary, and the parse in
 * `host-client.ts` deliberately stops at the *envelope* (`status`, `error`) so a newer host
 * can add fields to the MCP payload without breaking an older agent.
 */
function asCallOutput(value: unknown): HostMcpToolCallOutput | undefined {
  if (typeof value !== "object" || value === null) return undefined;
  const candidate = value as Partial<HostMcpToolCallOutput>;
  if (typeof candidate.serverId !== "string" || typeof candidate.tool !== "string") return undefined;
  if (typeof candidate.isError !== "boolean" || typeof candidate.text !== "string") return undefined;
  return {
    serverId: candidate.serverId,
    tool: candidate.tool,
    isError: candidate.isError,
    text: candidate.text,
    content: Array.isArray(candidate.content) ? candidate.content : [],
    ...(candidate.structuredContent === undefined ? {} : { structuredContent: candidate.structuredContent }),
  };
}

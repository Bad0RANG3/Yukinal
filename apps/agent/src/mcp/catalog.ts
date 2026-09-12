/**
 * Registering the host's MCP catalog into the `ToolRegistry` (ADR 0014).
 *
 * The split of responsibility is the point of this file:
 *
 * - the **host** decides which servers exist, that they are running, and what their tools are
 *   called (it owns the processes);
 * - the **agent** decides what a discovered tool means — its risk, its local input contract,
 *   and its provenance — and registers it.
 *
 * Nothing here starts a server, retries a dead one, or promotes a tool's trust. A failure is
 * reported with the server id and the host's own message, because "MCP is unavailable" without
 * saying *which* server and *why* is not something a user can act on.
 */

import type { HostMcpCatalogFailure, HostMcpCatalogResponse } from "@yukinal/shared";

import type { ToolRegistry } from "../tools/registry.js";
import type { ToolDeclaration } from "@yukinal/shared";

import { mcpToolFromCatalog, type McpToolOptions } from "./tool.js";

/**
 * How long the agent waits for the catalog before giving up and starting without MCP.
 *
 * The host caps the part it controls (`CATALOG_START_BUDGET`, 4s, for servers it has to
 * start), and the sidecar's handshake budget is 10s. This is the agent's own deadline: an
 * unresponsive host must not keep the agent from answering at all, and MCP tools that arrive
 * after this are simply not part of this session.
 */
export const MCP_CATALOG_BUDGET_MS = 8_000;

export interface RegisterCatalogOptions extends McpToolOptions {
  /** Where failures go. Defaults to dropping them, which is what a library should do. */
  log?: (message: string, meta?: Record<string, unknown>) => void;
}

export interface CatalogRegistration {
  /** Names registered, in catalog order. */
  readonly registered: string[];
  /** Servers the host reported as unusable, verbatim. */
  readonly failures: HostMcpCatalogFailure[];
  /** Entries this agent refused (a name it cannot accept, a duplicate, a broken declaration). */
  readonly rejected: { name: string; reason: string }[];
}

/**
 * Register every tool in a catalog response.
 *
 * `options` is required, not defaulted: every registered tool needs a way to reach the host
 * (`executeOnHost`). Registering tools that cannot run would put them in `agent.list_tools`
 * and in the model's prompt while every call failed with `internal`.
 *
 * One bad entry costs that entry, never the catalog: a server that declares a tool with an
 * unusable name must not take the other servers' tools down with it, and it must not be
 * silently skipped either — it lands in `rejected` with the reason.
 */
export function registerCatalog(
  registry: ToolRegistry,
  catalog: HostMcpCatalogResponse,
  options: RegisterCatalogOptions,
): CatalogRegistration {
  const registered: string[] = [];
  const rejected: { name: string; reason: string }[] = [];
  const log = options.log ?? ((): void => undefined);

  for (const failure of catalog.failures) {
    // The host's message is already the actionable one (it names the server, the exit code and
    // why it will not be restarted). Passed through rather than re-worded.
    log(`mcp server "${failure.serverId}" is unavailable (${failure.code}): ${failure.message}`, {
      serverId: failure.serverId,
      code: failure.code,
    });
  }

  for (const server of catalog.servers) {
    for (const entry of server.tools) {
      let declaration: ToolDeclaration;
      try {
        declaration = registry.register(mcpToolFromCatalog(entry, options));
      } catch (error) {
        const reason = error instanceof Error ? error.message : String(error);
        rejected.push({ name: entry.name, reason });
        log(`refused to register MCP tool "${entry.name}": ${reason}`, {
          serverId: server.serverId,
          tool: entry.tool,
        });
        continue;
      }
      registered.push(declaration.name);
    }
  }

  return { registered, failures: catalog.failures, rejected };
}

export interface LoadCatalogOptions extends RegisterCatalogOptions {
  registry: ToolRegistry;
  /**
   * Only the one method, not a whole `HostRpcClient`: what this function needs from the host is
   * "give me the catalog", and a test can answer that without a transport.
   */
  fetchCatalog(signal?: AbortSignal): Promise<HostMcpCatalogResponse>;
  /** Defaults to [`MCP_CATALOG_BUDGET_MS`]; tests set it low. */
  budgetMs?: number;
}

/**
 * Ask the host for the catalog and register what comes back.
 *
 * Returns `undefined` when the host did not give us a catalog at all — an older host that does
 * not know `host.mcp.catalog`, or one that is unreachable. That is **not** the same as "the
 * catalog is empty": the caller must not turn it into `capabilities.mcp: true` with zero tools,
 * and it must not retry either. There is no automatic recovery here for the same reason there is
 * none for a crashed server: a retry loop against a host that just refused to answer is how a
 * sidecar turns into a busy process.
 */
export async function loadCatalogFromHost(
  options: LoadCatalogOptions,
): Promise<CatalogRegistration | undefined> {
  const log = options.log ?? ((): void => undefined);
  const budgetMs = options.budgetMs ?? MCP_CATALOG_BUDGET_MS;
  const controller = new AbortController();
  const timer = setTimeout(() => {
    controller.abort(new Error(`mcp catalog request exceeded ${budgetMs}ms`));
  }, budgetMs);
  timer.unref?.();

  try {
    const catalog = await options.fetchCatalog(controller.signal);
    const registration = registerCatalog(options.registry, catalog, options);
    log("mcp catalog loaded", {
      servers: catalog.servers.map((server) => server.serverId),
      tools: registration.registered,
      failures: registration.failures.length,
      rejected: registration.rejected.length,
    });
    return registration;
  } catch (error) {
    // No catalog: MCP stays unimplemented for this session, which is what `capabilities.mcp`
    // must then say (ADR 0014).
    log("mcp catalog unavailable; continuing without MCP tools", {
      reason: error instanceof Error ? error.message : String(error),
    });
    return undefined;
  } finally {
    clearTimeout(timer);
  }
}

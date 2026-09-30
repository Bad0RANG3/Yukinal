import type { Server, ToolTarget } from "@yukinal/shared";

/** Explicit select value for an intentionally local task. */
export const LOCAL_TASK_TARGET = "__yukinal_local_target__";

/**
 * Resolve the goal form's explicit target selection. An empty or stale server
 * selection stays unresolved instead of silently turning into a local task.
 */
export function resolveInvestigationTaskTarget(
  selection: string,
  servers: readonly Server[],
): ToolTarget | undefined {
  if (selection === LOCAL_TASK_TARGET) return { host: "local", environment: "local" };
  if (!selection) return undefined;

  const server = servers.find((candidate) => candidate.id === selection);
  if (!server) return undefined;

  return {
    host: "remote",
    serverId: server.id,
    environment: server.metadata.environment,
  };
}

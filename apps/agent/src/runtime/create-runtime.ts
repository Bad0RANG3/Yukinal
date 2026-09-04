/**
 * Composition root for the agent runtime.
 *
 * Split out of `index.ts` so tests can build a runtime without attaching to stdio.
 * Startup order mirrors the layering: registry -> permissions -> context -> loop -> rpc.
 */

import {
  DEVELOPMENT_POLICY,
  LOCAL_POLICY,
  PRODUCTION_POLICY,
  STAGING_POLICY,
  type ToolDeclaration,
} from "@yukinal/shared";

import { createLogger, type AgentLogger } from "../config.js";
import { ContextEngine } from "../context/context-engine.js";
import { createEmptyContextSource } from "../context/empty-source.js";
import { PermissionEngine } from "../permissions/permission-engine.js";
import { RpcRouter } from "../rpc/router.js";
import { AgentLoop } from "./agent-loop.js";
import { dockerInspectTool } from "../tools/builtin/docker-inspect.js";
import { dockerLogsTool } from "../tools/builtin/docker-logs.js";
import { dockerPsTool } from "../tools/builtin/docker-ps.js";
import { serverInfoTool } from "../tools/builtin/server-info.js";
import { systemEchoTool } from "../tools/builtin/system-echo.js";
import { ToolRegistry } from "../tools/registry.js";
import type { HostRpcClient } from "../transport/host-client.js";

export const BUILTIN_POLICY_IDS = [
  LOCAL_POLICY.id,
  DEVELOPMENT_POLICY.id,
  STAGING_POLICY.id,
  PRODUCTION_POLICY.id,
];

export interface Runtime {
  registry: ToolRegistry;
  permission: PermissionEngine;
  context: ContextEngine;
  loop: AgentLoop;
  router: RpcRouter;
  log: AgentLogger;
  declarations: ToolDeclaration[];
}

export function createRuntime(options: { log?: AgentLogger; hostToolClient?: HostRpcClient } = {}): Runtime {
  const log = options.log ?? createLogger({ level: "info", scope: "agent" });

  const registry = new ToolRegistry();
  const declarations = [registry.register(systemEchoTool)];
  if (options.hostToolClient) {
    declarations.push(registry.register(serverInfoTool(options.hostToolClient)));
    declarations.push(registry.register(dockerPsTool(options.hostToolClient)));
    declarations.push(registry.register(dockerLogsTool(options.hostToolClient)));
    declarations.push(registry.register(dockerInspectTool(options.hostToolClient)));
  }

  const permission = new PermissionEngine();
  // Empty until Rust feeds real rows through the IPC-backed source.
  const context = new ContextEngine(createEmptyContextSource());
  // no provider yet, so loop.start() refuses instead of faking.
  const loop = new AgentLoop({ registry, permission, context });

  const router = new RpcRouter({
    registry,
    loop,
    log: log.child("rpc"),
    policyIds: BUILTIN_POLICY_IDS,
  });

  return { registry, permission, context, loop, router, log, declarations };
}

/**
 * Composition root for the agent runtime.
 *
 * Split out of `index.ts` so tests can build a runtime without attaching to stdio.
 * Startup order mirrors the layering: registry -> permissions -> context -> loop -> rpc.
 */

import type { ToolDeclaration } from "@yukinal/shared";

import { createLogger, type AgentLogger } from "../config.js";
import { ContextEngine } from "../context/context-engine.js";
import { createEmptyContextSource } from "../context/empty-source.js";
import { createHostContextSource } from "../context/host-context-source.js";
import { PermissionEngine } from "../permissions/permission-engine.js";
import { KNOWN_POLICY_IDS } from "../permissions/policy-registry.js";
import { RpcRouter } from "../rpc/router.js";
import { AgentLoop, type AgentLoopDeps } from "./agent-loop.js";
import { dockerInspectTool } from "../tools/builtin/docker-inspect.js";
import { dockerLogsTool } from "../tools/builtin/docker-logs.js";
import { dockerPsTool } from "../tools/builtin/docker-ps.js";
import { dockerRestartTool } from "../tools/builtin/docker-restart.js";
import { filesystemEditTool } from "../tools/builtin/filesystem-edit.js";
import { filesystemReadTool } from "../tools/builtin/filesystem-read.js";
import { filesystemWriteTool } from "../tools/builtin/filesystem-write.js";
import { serverInfoTool } from "../tools/builtin/server-info.js";
import { systemEchoTool } from "../tools/builtin/system-echo.js";
import { ToolRegistry } from "../tools/registry.js";
import type { HostRpcClient } from "../transport/host-client.js";

export interface Runtime {
  registry: ToolRegistry;
  permission: PermissionEngine;
  context: ContextEngine;
  loop: AgentLoop;
  router: RpcRouter;
  log: AgentLogger;
  declarations: ToolDeclaration[];
}

export function createRuntime(
  options: {
    log?: AgentLogger;
    hostToolClient?: HostRpcClient;
    maxRunMs?: number;
    /** Forwarded to the loop verbatim; tests use it to read the ledger a run wrote. */
    createTrace?: AgentLoopDeps["createTrace"];
  } = {},
): Runtime {
  const log = options.log ?? createLogger({ level: "info", scope: "agent" });

  const registry = new ToolRegistry();
  const declarations = [registry.register(systemEchoTool)];
  if (options.hostToolClient) {
    declarations.push(registry.register(serverInfoTool(options.hostToolClient)));
    declarations.push(registry.register(dockerPsTool(options.hostToolClient)));
    declarations.push(registry.register(dockerLogsTool(options.hostToolClient)));
    declarations.push(registry.register(dockerInspectTool(options.hostToolClient)));
    declarations.push(registry.register(dockerRestartTool(options.hostToolClient)));
    declarations.push(registry.register(filesystemReadTool(options.hostToolClient)));
    declarations.push(registry.register(filesystemWriteTool(options.hostToolClient)));
    // 读-改-写是**独立**的一项能力，不是 write 的开关：write 覆盖整个文件，edit 要求
    // 内容与刚读过的一致才替换。两者都给，模型才能自己选「要看清楚再改」还是「整份重写」。
    declarations.push(registry.register(filesystemEditTool(options.hostToolClient)));
  }

  const permission = new PermissionEngine();
  const context = new ContextEngine(
    options.hostToolClient ? createHostContextSource(options.hostToolClient) : createEmptyContextSource(),
  );
  // The router supplies a configured provider for each run; the loop refuses
  // direct calls without one instead of faking a response.
  const loop = new AgentLoop({
    registry,
    permission,
    context,
    maxRunMs: options.maxRunMs,
    createTrace: options.createTrace,
  });

  const router = new RpcRouter({
    registry,
    loop,
    log: log.child("rpc"),
    // The registry, not a second hand-written list: the ids `system.describe` reports
    // are exactly the ids a run request may name.
    policyIds: [...KNOWN_POLICY_IDS],
  });

  return { registry, permission, context, loop, router, log, declarations };
}

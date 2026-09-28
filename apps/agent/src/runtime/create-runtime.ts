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
import { filesystemBackupTool } from "../tools/builtin/filesystem-backup.js";
import { filesystemBackupListTool } from "../tools/builtin/filesystem-backup-list.js";
import { filesystemBackupRetentionTool } from "../tools/builtin/filesystem-backup-retention.js";
import { filesystemBackupCleanupTool } from "../tools/builtin/filesystem-backup-cleanup.js";
import { filesystemReadTool } from "../tools/builtin/filesystem-read.js";
import { filesystemRestoreTool } from "../tools/builtin/filesystem-restore.js";
import { filesystemWriteTool } from "../tools/builtin/filesystem-write.js";
import { investigationEvidenceTool } from "../tools/builtin/investigation-evidence.js";
import { investigationEvidenceSearchTool } from "../tools/builtin/investigation-evidence-search.js";
import { investigationEvidenceCompareTool } from "../tools/builtin/investigation-evidence-compare.js";
import { investigationEvidenceCorrelateTool } from "../tools/builtin/investigation-evidence-correlate.js";
import { investigationEvidenceTriageTool } from "../tools/builtin/investigation-evidence-triage.js";
import { investigationRetentionPreviewTool } from "../tools/builtin/investigation-retention-preview.js";
import { investigationFindingTool } from "../tools/builtin/investigation-finding.js";
import { investigationBriefTool } from "../tools/builtin/investigation-brief.js";
import { investigationPlanTool } from "../tools/builtin/investigation-plan.js";
import { investigationPlaybookTool } from "../tools/builtin/investigation-playbook.js";
import { investigationArtifactTool } from "../tools/builtin/investigation-artifact.js";
import { packageInspectTool } from "../tools/builtin/package-inspect.js";
import { packageInstallTool } from "../tools/builtin/package-install.js";
import { serverInfoTool } from "../tools/builtin/server-info.js";
import { serverLogsTool } from "../tools/builtin/server-logs.js";
import { serverServicesTool } from "../tools/builtin/server-services.js";
import { systemEchoTool } from "../tools/builtin/system-echo.js";
import { systemdInspectTool } from "../tools/builtin/systemd-inspect.js";
import { systemdRestartTool } from "../tools/builtin/systemd-restart.js";
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
    declarations.push(registry.register(serverLogsTool(options.hostToolClient)));
    declarations.push(registry.register(serverServicesTool(options.hostToolClient)));
    declarations.push(registry.register(dockerPsTool(options.hostToolClient)));
    declarations.push(registry.register(dockerLogsTool(options.hostToolClient)));
    declarations.push(registry.register(dockerInspectTool(options.hostToolClient)));
    declarations.push(registry.register(dockerRestartTool(options.hostToolClient)));
    declarations.push(registry.register(systemdInspectTool(options.hostToolClient)));
    declarations.push(registry.register(systemdRestartTool(options.hostToolClient)));
    declarations.push(registry.register(packageInspectTool(options.hostToolClient)));
    declarations.push(registry.register(packageInstallTool(options.hostToolClient)));
    declarations.push(registry.register(filesystemReadTool(options.hostToolClient)));
    declarations.push(registry.register(filesystemWriteTool(options.hostToolClient)));
    declarations.push(registry.register(filesystemBackupTool(options.hostToolClient)));
    declarations.push(registry.register(filesystemBackupListTool(options.hostToolClient)));
    declarations.push(registry.register(filesystemBackupRetentionTool(options.hostToolClient)));
    declarations.push(registry.register(filesystemBackupCleanupTool(options.hostToolClient)));
    declarations.push(registry.register(filesystemRestoreTool(options.hostToolClient)));
    // 读-改-写是**独立**的一项能力，不是 write 的开关：write 覆盖整个文件，edit 要求
    // 内容与刚读过的一致才替换。两者都给，模型才能自己选「要看清楚再改」还是「整份重写」。
    declarations.push(registry.register(filesystemEditTool(options.hostToolClient)));
    declarations.push(registry.register(investigationEvidenceTool(options.hostToolClient)));
    declarations.push(registry.register(investigationEvidenceSearchTool(options.hostToolClient)));
    declarations.push(registry.register(investigationEvidenceCompareTool(options.hostToolClient)));
    declarations.push(registry.register(investigationEvidenceCorrelateTool(options.hostToolClient)));
    declarations.push(registry.register(investigationEvidenceTriageTool(options.hostToolClient)));
    declarations.push(registry.register(investigationRetentionPreviewTool(options.hostToolClient)));
    declarations.push(registry.register(investigationFindingTool(options.hostToolClient)));
    declarations.push(registry.register(investigationBriefTool(options.hostToolClient)));
    declarations.push(registry.register(investigationPlanTool(options.hostToolClient)));
    declarations.push(registry.register(investigationPlaybookTool(options.hostToolClient)));
    declarations.push(registry.register(investigationArtifactTool(options.hostToolClient)));
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
    recordEvidence: options.hostToolClient
      ? (evidence, signal) => options.hostToolClient!.recordEvidence({ evidence }, signal)
      : undefined,
    recordArtifact: options.hostToolClient
      ? (request, signal) => options.hostToolClient!.recordArtifact(request, signal)
      : undefined,
    checkPlan: options.hostToolClient
      ? (request, signal) => options.hostToolClient!.checkPlan(request, signal)
      : undefined,
    recordPlanStepResult: options.hostToolClient
      ? (request, signal) => options.hostToolClient!.recordPlanStepResult(request, signal)
      : undefined,
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

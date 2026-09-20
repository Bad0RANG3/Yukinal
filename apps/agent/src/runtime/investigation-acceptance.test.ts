/**
 * Acceptance fixtures for the autonomous investigation/deployment loop.
 *
 * This is deliberately a memory-only host. It exercises the real AgentLoop,
 * registry, permission engine, plan gate, evidence recorder and artifact path,
 * while never opening a VM, socket to a target server, or bridged network.
 */

import assert from "node:assert/strict";
import test from "node:test";

import type {
  AgentPermissionMode,
  AgentRunRequest,
  AgentStreamEvent,
  DecisionBrief,
  Evidence,
  Finding,
  HostPlanStepResultRequest,
  InvestigationArtifact,
  InvestigationPlan,
  InvestigationPlanStep,
  ToolTarget,
} from "@yukinal/shared";
import type { ChatRequest, LLMProvider } from "@yukinal/provider-sdk";

import type { AgentLogger } from "../config.js";
import { HostRpcClient } from "../transport/host-client.js";
import { createRuntime } from "./create-runtime.js";

const noop = (): void => {};
const silent: AgentLogger = { debug: noop, info: noop, warn: noop, error: noop, child: () => silent };
const TARGET: ToolTarget = { host: "remote", serverId: "srv_fixture", environment: "staging" };
const CONFIG_PATH = "/etc/yukinal.conf";
const CONTAINER = "api_1";
const SERVICE = "nginx.service";
const PACKAGE_MANAGER = "apt";
const PACKAGE = "nginx";
const PACKAGE_VERSION = "1.27.0-1";
const REVISION = "a".repeat(64);
const BACKUP_PATH = "/etc/.yukinal-backup-fixture-" + "b".repeat(32);
const NOW = "2026-09-20T00:00:00.000Z";

type Scenario = "success" | "verification_failure" | "rollback_rejected" | "deployment_sequence" | "service_restart" | "package_install" | "backup_cleanup" | "readonly_health";

interface Fixture {
  readonly taskId: string;
  readonly scenario: Scenario;
  readonly host: HostRpcClient;
  readonly audit: Array<{ method: string; params: Record<string, unknown> }>;
  readonly planChecks: Array<Record<string, unknown>>;
  readonly stepResults: HostPlanStepResultRequest[];
  readonly executions: Array<Record<string, unknown>>;
  readonly evidence: Evidence[];
  readonly findings: Finding[];
  readonly briefs: DecisionBrief[];
  readonly artifacts: InvestigationArtifact[];
  backupAvailable: boolean;
  plan?: InvestigationPlan;
}

type ScriptItem =
  | { name: string; input: Record<string, unknown> }
  | ((fixture: Fixture, request: ChatRequest) => { name: string; input: Record<string, unknown> });

function parseFrame(frame: string): { id: number; method: string; params: Record<string, unknown> } {
  const value = JSON.parse(frame) as { id: number; method: string; params?: Record<string, unknown> };
  return { id: value.id, method: value.method, params: value.params ?? {} };
}

function answer(host: HostRpcClient, id: number, result: unknown): void {
  host.handleIncoming({ jsonrpc: "2.0", id, result });
}

function modelEvidenceId(request: ChatRequest): string | undefined {
  return modelEvidenceIds(request).at(-1);
}

function modelEvidenceIds(request: ChatRequest): string[] {
  return request.messages
    .filter((message): message is Extract<ChatRequest["messages"][number], { role: "tool" }> => message.role === "tool")
    .flatMap((message) => [...message.content.matchAll(/证据已保存：([^\s]+)/g)].map((match) => match[1]))
    .filter((id): id is string => Boolean(id));
}

function modelFindingId(request: ChatRequest): string | undefined {
  const ids = request.messages
    .filter((message): message is Extract<ChatRequest["messages"][number], { role: "tool" }> => message.role === "tool")
    .flatMap((message) => [...message.content.matchAll(/"id":\s*"(finding_[^"]+)"/g)].map((match) => match[1]))
    .filter((id): id is string => Boolean(id));
  return ids.at(-1);
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

function currentStep(fixture: Fixture): InvestigationPlanStep | undefined {
  const plan = fixture.plan;
  return plan?.currentStepId === undefined ? undefined : plan.steps.find((step) => step.id === plan.currentStepId);
}

function failureArtifact(fixture: Fixture, request: HostPlanStepResultRequest, step: InvestigationPlanStep): InvestigationArtifact {
  return {
    id: `artifact_failure_${request.planId}_${request.stepId}`,
    taskId: fixture.taskId,
    planId: request.planId,
    planStepId: request.stepId,
    phase: "recovery",
    kind: "failure",
    status: "failed",
    title: "计划步骤失败",
    summary: request.outputSummary ?? "计划步骤未完成，等待用户决定下一步",
    content: {
      code: "command_failed",
      status: request.status,
      retryable: request.retryable,
      planId: request.planId,
      stepId: request.stepId,
    },
    evidenceIds: step.evidenceIds,
    createdAt: NOW,
    updatedAt: NOW,
  };
}

function advancePlan(fixture: Fixture, request: HostPlanStepResultRequest): void {
  if (!fixture.plan) return;
  const plan = clone(fixture.plan);
  const index = plan.steps.findIndex((step) => step.id === request.stepId);
  if (index < 0) return;
  const step = plan.steps[index];
  if (!step) return;
  step.attempts += 1;
  step.endedAt = NOW;
  if (request.status === "success") {
    step.status = "succeeded";
    const next = plan.steps[index + 1];
    if (next) {
      next.status = "running";
      next.startedAt = NOW;
      plan.currentStepId = next.id;
    } else {
      plan.currentStepId = undefined;
      plan.status = "completed";
    }
  } else {
    step.status = "blocked";
    fixture.artifacts.push(failureArtifact(fixture, request, step));
  }
  plan.updatedAt = NOW;
  fixture.plan = plan;
}

function createFixture(taskId: string, scenario: Scenario): Fixture {
  const state: Omit<Fixture, "host"> & { host?: HostRpcClient } = {
    taskId,
    scenario,
    audit: [],
    planChecks: [],
    stepResults: [],
    executions: [],
    evidence: [],
    findings: [],
    briefs: [],
    artifacts: [],
    backupAvailable: true,
  };
  let host!: HostRpcClient;
  let serverInfoReads = 0;
  host = new HostRpcClient((frame) => {
    const sent = parseFrame(frame);
    state.audit.push({ method: sent.method, params: sent.params });

    if (sent.method === "host.context.fetch") {
      answer(host, sent.id, { status: "not_found" });
      return;
    }
    if (sent.method === "host.investigation.plan.record") {
      const plan = clone(sent.params.plan as InvestigationPlan);
      state.plan = plan;
      answer(host, sent.id, { recorded: true, plan });
      return;
    }
    if (sent.method === "host.investigation.plan.check") {
      state.planChecks.push(sent.params);
      const step = currentStep(state as Fixture);
      const toolName = String(sent.params.toolName);
      const input = sent.params.input;
      if (!step || !state.plan) {
        answer(host, sent.id, {
          status: "deviation",
          deviation: {
            code: "no_active_step",
            action: "wait_user",
            message: "fixture has no active plan step",
            toolName,
            at: NOW,
          },
        });
        return;
      }
      const bindings = step.inputBindings ?? {};
      const inputObject = input && typeof input === "object" && !Array.isArray(input) ? input as Record<string, unknown> : {};
      const bindingMismatch = Object.entries(bindings).some(([key, value]) => inputObject[key] !== value);
      if (bindingMismatch || !step.allowedTools.includes(toolName)) {
        answer(host, sent.id, {
          status: "deviation",
          deviation: {
            code: bindingMismatch ? "binding_mismatch" : "tool_not_allowed",
            action: "wait_user",
            message: "fixture rejected a tool or exact input binding",
            toolName,
            at: NOW,
            planId: state.plan.id,
            stepId: step.id,
          },
        });
        return;
      }
      answer(host, sent.id, {
        status: "allowed",
        planId: state.plan.id,
        stepId: step.id,
        stepKind: step.kind,
        evidenceIds: step.evidenceIds,
        requiresApproval: step.requiresApproval,
      });
      return;
    }
    if (sent.method === "host.tool.execute") {
      const toolName = String(sent.params.toolName);
      state.executions.push(sent.params);
      if (toolName === "server.info") {
        serverInfoReads += 1;
        answer(host, sent.id, {
          status: "success",
          output: {
            id: `snapshot_fixture_${serverInfoReads}`,
            serverId: "srv_fixture",
            collectedAt: NOW,
            health: "healthy",
            capabilities: { linux: true, docker: true, systemd: true },
          },
        });
        return;
      }
      if (toolName === "server.logs") {
        answer(host, sent.id, {
          status: "success",
          output: {
            source: "journalctl",
            lines: [{ text: "2026-09-20T00:00:00Z fixture service ready", level: "info" }],
          },
        });
        return;
      }
      if (toolName === "server.services") {
        answer(host, sent.id, {
          status: "success",
          output: {
            source: "systemd",
            services: [{ name: "fixture.service", state: "running", status: "active/running", description: "Fixture service" }],
          },
        });
        return;
      }
      if (toolName === "docker.ps") {
        answer(host, sent.id, {
          status: "success",
          output: {
            available: true,
            containers: [{ name: CONTAINER, image: "yukinal/api:fixture", state: "running", status: "Up 1 minute", restartCount: 1 }],
          },
        });
        return;
      }
      if (toolName === "filesystem.read") {
        const step = currentStep(state as Fixture);
        if (scenario === "verification_failure" && step?.kind === "verification") {
          answer(host, sent.id, {
            status: "failed",
            error: { code: "execution_failed", message: "fixture verification found MODE=legacy", retryable: false },
          });
        } else {
          answer(host, sent.id, {
            status: "success",
            output: { path: CONFIG_PATH, content: "MODE=managed\n", truncated: false, revision: REVISION },
          });
        }
        return;
      }
      if (toolName === "filesystem.backup") {
        answer(host, sent.id, {
          status: "success",
          output: { path: CONFIG_PATH, backupPath: BACKUP_PATH, revision: REVISION, bytesBackedUp: 13 },
        });
        return;
      }
      if (toolName === "filesystem.backup.list") {
        answer(host, sent.id, {
          status: "success",
          output: {
            backups: state.backupAvailable ? [{
              id: "backup_fixture",
              serverId: "srv_fixture",
              taskId,
              path: CONFIG_PATH,
              backupPath: BACKUP_PATH,
              revision: REVISION,
              bytesBackedUp: 13,
              status: "available",
              createdAt: NOW,
              updatedAt: NOW,
            }] : [],
            truncated: false,
          },
        });
        return;
      }
      if (toolName === "filesystem.backup.cleanup") {
        state.backupAvailable = false;
        answer(host, sent.id, {
          status: "success",
          output: { path: CONFIG_PATH, backupPath: BACKUP_PATH, revision: REVISION, bytesDeleted: 13 },
        });
        return;
      }
      if (toolName === "filesystem.edit") {
        answer(host, sent.id, {
          status: "success",
          output: { path: CONFIG_PATH, revision: "b".repeat(64), bytesBefore: 13, bytesAfter: 14, lineDelta: 0 },
        });
        return;
      }
      if (toolName === "filesystem.restore") {
        answer(host, sent.id, {
          status: "success",
          output: { path: CONFIG_PATH, backupPath: BACKUP_PATH, revision: REVISION, bytesBefore: 14, bytesAfter: 13 },
        });
        return;
      }
      if (toolName === "docker.inspect") {
        answer(host, sent.id, {
          status: "success",
          output: {
            id: "container_fixture",
            name: CONTAINER,
            image: "yukinal/api:fixture",
            state: "running",
            status: "Up 1 minute",
            restartCount: 1,
            health: "healthy",
          },
        });
        return;
      }
      if (toolName === "docker.logs") {
        answer(host, sent.id, {
          status: "success",
          output: { container: CONTAINER, lines: ["2026-09-20T00:00:00Z fixture ready"], truncated: false },
        });
        return;
      }
      if (toolName === "docker.restart") {
        answer(host, sent.id, {
          status: "success",
          output: { container: CONTAINER, restarted: true },
        });
        return;
      }
      if (toolName === "systemd.inspect") {
        answer(host, sent.id, {
          status: "success",
          output: {
            service: SERVICE,
            loadState: "loaded",
            activeState: "active",
            subState: "running",
            description: "Fixture web service",
          },
        });
        return;
      }
      if (toolName === "systemd.restart") {
        answer(host, sent.id, {
          status: "success",
          output: { service: SERVICE, restarted: true },
        });
        return;
      }
      if (toolName === "package.inspect") {
        answer(host, sent.id, {
          status: "success",
          output: { manager: PACKAGE_MANAGER, package: PACKAGE, installed: true, version: PACKAGE_VERSION },
        });
        return;
      }
      if (toolName === "package.install") {
        answer(host, sent.id, {
          status: "success",
          output: { manager: PACKAGE_MANAGER, package: PACKAGE, version: PACKAGE_VERSION, installed: true },
        });
        return;
      }
      answer(host, sent.id, {
        status: "failed",
        error: { code: "not_found", message: `fixture has no output for ${toolName}`, retryable: false },
      });
      return;
    }
    if (sent.method === "host.investigation.evidence.record") {
      state.evidence.push(clone(sent.params.evidence as Evidence));
      answer(host, sent.id, { recorded: true });
      return;
    }
    if (sent.method === "host.investigation.finding.record") {
      const finding = clone(sent.params.finding as Finding);
      if (!finding.evidenceIds.every((id) => state.evidence.some((evidence) => evidence.id === id))) {
        answer(host, sent.id, {
          recorded: false,
          error: { code: "invalid_input", message: "fixture finding references unknown evidence", retryable: false },
        });
        return;
      }
      state.findings.push(finding);
      answer(host, sent.id, { recorded: true, finding });
      return;
    }
    if (sent.method === "host.investigation.brief.record") {
      const brief = clone(sent.params.brief as DecisionBrief);
      const findingIds = new Set(state.findings.map((finding) => finding.id));
      const evidenceIds = new Set(state.evidence.map((evidence) => evidence.id));
      const references = [
        ...brief.findingIds,
        ...brief.options.flatMap((option) => option.findingIds),
      ];
      const evidenceReferences = brief.options.flatMap((option) => option.evidenceIds);
      if (!references.every((id) => findingIds.has(id)) || !evidenceReferences.every((id) => evidenceIds.has(id))) {
        answer(host, sent.id, {
          recorded: false,
          error: { code: "invalid_input", message: "fixture brief references unknown evidence or finding", retryable: false },
        });
        return;
      }
      state.briefs.push(brief);
      answer(host, sent.id, { recorded: true, brief });
      return;
    }
    if (sent.method === "host.investigation.artifact.record") {
      const artifact = clone(sent.params.artifact as InvestigationArtifact);
      state.artifacts.push(artifact);
      answer(host, sent.id, { recorded: true, artifact });
      return;
    }
    if (sent.method === "host.investigation.plan.step_result") {
      const request = clone(sent.params as unknown as HostPlanStepResultRequest);
      state.stepResults.push(request);
      advancePlan(state as Fixture, request);
      answer(host, sent.id, { recorded: true, plan: state.plan });
      return;
    }
    throw new Error(`unexpected fixture RPC method: ${sent.method}`);
  });
  (state as { host: HostRpcClient }).host = host;
  return state as Fixture;
}

function providerFor(fixture: Fixture, script: ScriptItem[], finalText: string): LLMProvider {
  let turn = 0;
  return {
    id: "fixture-provider",
    model: "fixture-model",
    async listModels() {
      return [];
    },
    async *stream(request) {
      const item = script[turn++];
      if (!item) {
        yield { type: "text_delta", text: finalText };
        yield { type: "done", finishReason: "stop" };
        return;
      }
      const call = typeof item === "function" ? item(fixture, request) : item;
      yield {
        type: "tool_call",
        call: { id: `fixture_call_${turn}`, name: call.name.replaceAll(".", "__"), arguments: call.input },
      };
      yield { type: "done", finishReason: "tool_calls" };
    },
  };
}

function readonlyHealthScript(): ScriptItem[] {
  return [
    { name: "investigation.playbook", input: { template: "readonly_health" } },
    { name: "server.info", input: {} },
    { name: "server.logs", input: {} },
    { name: "server.services", input: {} },
    { name: "docker.ps", input: {} },
    (_fixture, request) => ({
      name: "investigation.finding",
      input: {
        title: "健康、日志、服务与容器状态已对齐",
        kind: "fact",
        statement: "fixture 返回了同一轮的服务器健康、日志、服务和 Docker 观测，当前未发现停止或失败组件",
        evidenceIds: modelEvidenceIds(request),
        confidence: "high",
      },
    }),
    (_fixture, request) => ({
      name: "investigation.brief",
      input: {
        findingIds: [modelFindingId(request) ?? "missing_finding"],
        options: [{
          title: "保持当前状态并持续观察",
          summary: "当前只读证据显示服务和容器均在运行，暂不执行远端变更",
          impact: "不改变目标状态，但不能替代更长时间窗口的监控",
          riskLevel: "low",
          evidenceIds: modelEvidenceIds(request),
          findingIds: [modelFindingId(request) ?? "missing_finding"],
          preview: "只保存本轮健康排查结果，不调用任何写入工具",
          verification: "在结束前重新读取 server.info，确认健康状态仍可获得",
          rollback: "没有远端写入，因此不需要回滚",
          requiresApproval: false,
        }],
      },
    }),
    { name: "server.info", input: {} },
  ];
}

function configEditScript(): ScriptItem[] {
  return [
    { name: "investigation.playbook", input: { template: "config_edit", path: CONFIG_PATH } },
    { name: "server.info", input: {} },
    { name: "filesystem.read", input: { path: CONFIG_PATH } },
    (_fixture, request) => ({
      name: "investigation.finding",
      input: {
        title: "配置已读取",
        kind: "fact",
        statement: "fixture 读取到受保护配置的当前内容",
        evidenceIds: [modelEvidenceId(request) ?? "missing_evidence"],
        confidence: "high",
      },
    }),
    (_fixture, request) => ({
      name: "investigation.brief",
      input: {
        findingIds: [modelFindingId(request) ?? "missing_finding"],
        options: [{
          title: "应用小范围配置修复",
          summary: "只替换一个已核对的配置片段",
          impact: "短暂影响配置读取",
          riskLevel: "medium",
          evidenceIds: [modelEvidenceId(request) ?? "missing_evidence"],
          findingIds: [modelFindingId(request) ?? "missing_finding"],
          preview: "仅替换一个精确字符串",
          verification: "重新读取文件并比较结果",
          rollback: "验证失败时另行生成反向编辑计划",
          requiresApproval: true,
        }],
      },
    }),
    { name: "filesystem.backup", input: { path: CONFIG_PATH } },
    {
      name: "filesystem.edit",
      input: {
        path: CONFIG_PATH,
        expectedRevision: REVISION,
        oldString: "MODE=managed",
        newString: "MODE=managed\n# fixture deployment",
      },
    },
    { name: "filesystem.read", input: { path: CONFIG_PATH } },
  ];
}

function deploymentSequenceScript(): ScriptItem[] {
  return [
    { name: "investigation.playbook", input: {
      template: "deploy_sequence",
      steps: [
        { operation: "edit_file", path: CONFIG_PATH },
        { operation: "restart_container", container: CONTAINER },
      ],
    } },
    { name: "server.info", input: {} },
    { name: "filesystem.read", input: { path: CONFIG_PATH } },
    { name: "docker.inspect", input: { container: CONTAINER } },
    { name: "docker.logs", input: { container: CONTAINER } },
    (_fixture, request) => ({
      name: "investigation.finding",
      input: {
        title: "部署前状态已核对",
        kind: "fact",
        statement: "fixture 已提供配置 revision、容器状态和日志样本",
        evidenceIds: [modelEvidenceId(request) ?? "missing_evidence"],
        confidence: "high",
      },
    }),
    (_fixture, request) => ({
      name: "investigation.brief",
      input: {
        findingIds: [modelFindingId(request) ?? "missing_finding"],
        options: [{
          title: "应用受保护配置并重启容器",
          summary: "先按 revision 编辑配置，再重启目标容器并逐项验证",
          impact: "配置生效会造成一次短暂容器重启",
          riskLevel: "high",
          evidenceIds: [modelEvidenceId(request) ?? "missing_evidence"],
          findingIds: [modelFindingId(request) ?? "missing_finding"],
          preview: "只修改一个精确配置片段并重启 api_1",
          verification: "重新读取配置并检查容器健康状态",
          rollback: "失败后另行生成反向编辑或恢复计划",
          requiresApproval: true,
        }],
      },
    }),
    { name: "filesystem.backup", input: { path: CONFIG_PATH } },
    {
      name: "filesystem.edit",
      input: {
        path: CONFIG_PATH,
        expectedRevision: REVISION,
        oldString: "MODE=managed",
        newString: "MODE=managed\n# fixture deployment",
      },
    },
    { name: "filesystem.read", input: { path: CONFIG_PATH } },
    { name: "docker.restart", input: { container: CONTAINER } },
    { name: "docker.inspect", input: { container: CONTAINER } },
  ];
}

function serviceRestartScript(): ScriptItem[] {
  return [
    { name: "investigation.playbook", input: {
      template: "systemd_restart",
      service: SERVICE,
    } },
    { name: "server.info", input: {} },
    { name: "systemd.inspect", input: { service: SERVICE } },
    (_fixture, request) => ({
      name: "investigation.finding",
      input: {
        title: "服务状态已核对",
        kind: "fact",
        statement: "fixture 已提供 systemd 服务的当前状态",
        evidenceIds: [modelEvidenceId(request) ?? "missing_evidence"],
        confidence: "high",
      },
    }),
    (_fixture, request) => ({
      name: "investigation.brief",
      input: {
        findingIds: [modelFindingId(request) ?? "missing_finding"],
        options: [{
          title: "重启受保护 systemd 服务",
          summary: "只重启已核对的 nginx.service，并在之后重新读取状态",
          impact: "服务会有一次短暂中断",
          riskLevel: "high",
          evidenceIds: [modelEvidenceId(request) ?? "missing_evidence"],
          findingIds: [modelFindingId(request) ?? "missing_finding"],
          preview: "仅调用 systemd.restart nginx.service",
          verification: "systemd.inspect 返回 active/running",
          rollback: "重启没有直接回退；失败后另行规划恢复动作",
          requiresApproval: true,
        }],
      },
    }),
    { name: "systemd.restart", input: { service: SERVICE } },
    { name: "systemd.inspect", input: { service: SERVICE } },
  ];
}

function packageInstallScript(): ScriptItem[] {
  return [
    { name: "investigation.playbook", input: {
      template: "package_install",
      manager: PACKAGE_MANAGER,
      package: PACKAGE,
      version: PACKAGE_VERSION,
    } },
    { name: "server.info", input: {} },
    { name: "package.inspect", input: { manager: PACKAGE_MANAGER, package: PACKAGE } },
    (_fixture, request) => ({
      name: "investigation.finding",
      input: {
        title: "包状态已核对",
        kind: "fact",
        statement: "fixture 已提供包管理器、包名和当前版本",
        evidenceIds: [modelEvidenceId(request) ?? "missing_evidence"],
        confidence: "high",
      },
    }),
    (_fixture, request) => ({
      name: "investigation.brief",
      input: {
        findingIds: [modelFindingId(request) ?? "missing_finding"],
        options: [{
          title: "安装受保护软件包",
          summary: "按已核对的 apt 包名和精确版本执行安装，再复查版本",
          impact: "可能安装依赖并改变目标运行环境",
          riskLevel: "high",
          evidenceIds: [modelEvidenceId(request) ?? "missing_evidence"],
          findingIds: [modelFindingId(request) ?? "missing_finding"],
          preview: "仅调用 package.install，不使用 sudo 或自由命令",
          verification: "package.inspect 返回目标版本",
          rollback: "安装没有自动回退；失败后另行规划移除或恢复方案",
          requiresApproval: true,
        }],
      },
    }),
    { name: "package.install", input: { manager: PACKAGE_MANAGER, package: PACKAGE, version: PACKAGE_VERSION } },
    { name: "package.inspect", input: { manager: PACKAGE_MANAGER, package: PACKAGE } },
  ];
}

function backupCleanupScript(): ScriptItem[] {
  return [
    { name: "investigation.playbook", input: {
      template: "backup_cleanup",
      path: CONFIG_PATH,
      backupPath: BACKUP_PATH,
      expectedRevision: REVISION,
    } },
    { name: "filesystem.backup.list", input: { path: CONFIG_PATH, status: "available" } },
    (_fixture, request) => ({
      name: "investigation.finding",
      input: {
        title: "恢复点账本已核对",
        kind: "fact",
        statement: "fixture 返回了当前任务拥有的可用恢复点元数据",
        evidenceIds: [modelEvidenceId(request) ?? "missing_evidence"],
        confidence: "high",
      },
    }),
    (_fixture, request) => ({
      name: "investigation.brief",
      input: {
        findingIds: [modelFindingId(request) ?? "missing_finding"],
        options: [{
          title: "删除已核对的恢复点",
          summary: "只删除宿主账本中同一任务、同一路径和 revision 的恢复副本",
          impact: "删除后不能用该副本恢复，需要重新创建备份",
          riskLevel: "medium",
          evidenceIds: [modelEvidenceId(request) ?? "missing_evidence"],
          findingIds: [modelFindingId(request) ?? "missing_finding"],
          preview: "先做远端一致性检查，再删除一个 host-owned sibling backup",
          verification: "再次读取 available 账本，确认该恢复点不再可用",
          rollback: "清理不可逆；需要恢复时重新读取目标并创建新备份",
          requiresApproval: true,
        }],
      },
    }),
    { name: "filesystem.backup.cleanup", input: {
      path: CONFIG_PATH,
      backupPath: BACKUP_PATH,
      expectedRevision: REVISION,
    } },
    { name: "filesystem.backup.list", input: { path: CONFIG_PATH, status: "available" } },
  ];
}

async function runFixture(
  fixture: Fixture,
  script: ScriptItem[],
  finalText: string,
  approvalDecision: "approve_once" | "reject" = "approve_once",
  permissionMode: AgentPermissionMode = "ask",
): Promise<{ events: AgentStreamEvent[]; result: Awaited<ReturnType<ReturnType<typeof createRuntime>["loop"]["start"]>> }> {
  const runtime = createRuntime({ log: silent, hostToolClient: fixture.host });
  const events: AgentStreamEvent[] = [];
  const resultPromise = runtime.loop.start(
    {
      runId: `run_${fixture.taskId}`,
      sessionId: `session_${fixture.taskId}`,
      taskId: fixture.taskId,
      prompt: "在受控 fixture 中完成一项有边界的配置维护任务",
      mode: "goal",
      permissionMode,
      target: TARGET,
      taskBudget: { maxSteps: 20, maxRunMs: 60_000, maxAttempts: 2 },
    } satisfies AgentRunRequest,
    {
      emit: (event) => {
        events.push(event);
        if (event.type === "agent.waiting_approval") {
          queueMicrotask(() => runtime.loop.respondApproval({
            runId: event.runId,
            approvalId: event.approval.approvalId,
            decision: approvalDecision,
            respondedAt: NOW,
          }));
        }
      },
    },
    providerFor(fixture, script, finalText),
  );
  return { events, result: await resultPromise };
}

test("acceptance fixture: a bounded config deployment succeeds with evidence, approval and verification", async () => {
  const fixture = createFixture("task_fixture_success", "success");
  const { events, result } = await runFixture(fixture, configEditScript(), "fixture deployment succeeded");

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.match(result.text, /fixture deployment succeeded/);
  assert.equal(fixture.plan?.status, "completed");
  assert.equal(fixture.executions.filter((call) => call.toolName === "filesystem.edit").length, 1);
  assert.equal(fixture.executions.filter((call) => call.toolName === "filesystem.backup").length, 1);
  assert.equal(fixture.evidence.length >= 3, true);
  assert.equal(
    fixture.evidence.every((evidence) => !evidence.sourceTool.startsWith("investigation.")),
    true,
    "investigation metadata must not be re-recorded as target evidence",
  );
  assert.equal(fixture.findings.length, 1);
  assert.equal(fixture.briefs.length, 1);
  assert.equal(
    fixture.findings[0]?.evidenceIds.every((id) => fixture.evidence.some((evidence) => evidence.id === id)),
    true,
  );
  assert.equal(fixture.artifacts.some((artifact) => artifact.kind === "baseline"), true);
  assert.equal(fixture.artifacts.some((artifact) => artifact.kind === "execution"), true);
  assert.equal(fixture.artifacts.some((artifact) => artifact.kind === "verification"), true);
  assert.equal(fixture.audit.some((entry) => entry.method === "host.investigation.plan.check"), true);
  const approval = events.find((event) => event.type === "agent.waiting_approval");
  assert(approval && approval.type === "agent.waiting_approval");
  const actionResult = events.find((event) => event.type === "agent.tool_result" && event.toolName === "filesystem.edit");
  assert(actionResult && actionResult.type === "agent.tool_result");
  assert.equal(actionResult.approvedBy, "user");
  assert.equal(actionResult.status, "success");
});

test("acceptance fixture: readonly health chains server, logs, services and Docker into evidence", async () => {
  const fixture = createFixture("task_fixture_readonly_health", "readonly_health");
  const { result } = await runFixture(fixture, readonlyHealthScript(), "fixture readonly health completed");

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.equal(fixture.plan?.status, "completed");
  assert.deepEqual(
    fixture.executions.map((call) => call.toolName),
    ["server.info", "server.logs", "server.services", "docker.ps", "server.info"],
  );
  assert.deepEqual(
    fixture.evidence.map((evidence) => evidence.sourceTool),
    ["server.info", "server.logs", "server.services", "docker.ps", "server.info"],
  );
  assert.equal(fixture.evidence.some((evidence) => evidence.kind === "log" && evidence.sourceTool === "server.logs"), true);
  assert.equal(fixture.evidence.some((evidence) => evidence.kind === "service" && evidence.sourceTool === "server.services"), true);
  assert.equal(fixture.findings.length, 1);
  assert.equal(fixture.briefs.length, 1);
  assert.equal(fixture.artifacts.some((artifact) => artifact.kind === "baseline"), true);
  assert.equal(fixture.artifacts.some((artifact) => artifact.kind === "verification"), true);
});

test("acceptance fixture: a verification failure stops the plan and leaves a recovery artifact", async () => {
  const fixture = createFixture("task_fixture_verification_failure", "verification_failure");
  const { result } = await runFixture(fixture, configEditScript(), "fixture verification failed; waiting for a decision");

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.equal(fixture.plan?.status, "active");
  assert.equal(fixture.plan?.steps.at(-1)?.status, "blocked");
  assert.equal(fixture.stepResults.at(-1)?.status, "failed");
  assert.equal(fixture.executions.filter((call) => call.toolName === "filesystem.edit").length, 1);
  assert.equal(fixture.executions.filter((call) => call.toolName === "filesystem.backup").length, 1);
  assert.equal(fixture.artifacts.some((artifact) => artifact.kind === "failure" && artifact.status === "failed"), true);
  assert.equal(fixture.artifacts.some((artifact) => artifact.kind === "verification"), false);
  assert.match(result.text, /fixture verification failed/);
});

test("acceptance fixture: an auto goal executes medium config actions without approval", async () => {
  const fixture = createFixture("task_fixture_auto_delegation", "success");
  const { events, result } = await runFixture(
    fixture,
    configEditScript(),
    "fixture delegated deployment succeeded",
    "approve_once",
    "auto",
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.equal(fixture.plan?.status, "completed");
  assert.equal(events.filter((event) => event.type === "agent.waiting_approval").length, 0);
  const actionResults = events.filter(
    (event): event is Extract<AgentStreamEvent, { type: "agent.tool_result" }> =>
      event.type === "agent.tool_result" && (event.toolName === "filesystem.backup" || event.toolName === "filesystem.edit"),
  );
  assert.equal(actionResults.length, 2);
  assert.equal(actionResults.every((event) => event.approvedBy === "agent" && event.status === "success"), true);
});

test("acceptance fixture: rejecting a separately planned guarded restore never reaches the host action", async () => {
  const fixture = createFixture("task_fixture_rollback_rejected", "rollback_rejected");
  const rollbackPlan: ScriptItem = {
    name: "investigation.plan",
    input: {
      steps: [{
        kind: "action",
        title: "Apply separately approved guarded restore",
        purpose: "Restore the host-owned backup after a failed verification",
        allowedTools: ["filesystem.restore"],
        inputBindings: { path: CONFIG_PATH, backupPath: BACKUP_PATH },
        idempotency: "conditional",
        riskLevel: "high",
        requiresBaseline: false,
        preconditions: ["The current file must still match expectedRevision.", "backupPath must have been returned by filesystem.backup for this file."],
        verificationCriteria: ["The previous setting is restored"],
        preview: "Preview only: restore the host-owned sibling backup",
        rollback: "Stop and ask the user for a new plan",
        evidenceIds: [],
        successCriteria: ["The guarded restore is accepted by the host"],
        requiresApproval: true,
        maxAttempts: 1,
      }],
    },
  };
  const guardedRestore: ScriptItem = {
    name: "filesystem.restore",
    input: {
      path: CONFIG_PATH,
      backupPath: BACKUP_PATH,
      expectedRevision: "b".repeat(64),
    },
  };
  const { events, result } = await runFixture(
    fixture,
    [rollbackPlan, guardedRestore],
    "用户拒绝回退，保持当前现场并等待新的决定",
    "reject",
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.equal(fixture.executions.some((call) => call.toolName === "filesystem.restore"), false);
  assert.equal(fixture.stepResults.at(-1)?.status, "failed");
  assert.equal(fixture.plan?.steps[0]?.status, "blocked");
  assert.equal(fixture.artifacts.some((artifact) => artifact.kind === "failure"), true);
  const rejected = events.find((event) => event.type === "agent.tool_result" && event.toolName === "filesystem.restore");
  assert(rejected && rejected.type === "agent.tool_result");
  assert.equal(rejected.status, "failed");
  assert.equal(rejected.approvedBy, undefined);
  assert.match(result.text, /用户拒绝回退/);
});

test("acceptance fixture: a bounded deployment sequence edits then restarts and verifies", async () => {
  const fixture = createFixture("task_fixture_deployment_sequence", "deployment_sequence");
  const { events, result } = await runFixture(
    fixture,
    deploymentSequenceScript(),
    "fixture deployment sequence succeeded",
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.equal(fixture.plan?.status, "completed");
  assert.equal(fixture.executions.filter((call) => call.toolName === "filesystem.backup").length, 1);
  assert.equal(fixture.executions.filter((call) => call.toolName === "filesystem.edit").length, 1);
  assert.equal(fixture.executions.filter((call) => call.toolName === "docker.restart").length, 1);
  assert.equal(fixture.executions.filter((call) => call.toolName === "docker.inspect").length, 2);
  assert.equal(fixture.executions.filter((call) => call.toolName === "docker.logs").length, 1);
  assert.equal(fixture.artifacts.some((artifact) => artifact.kind === "baseline"), true);
  assert.equal(fixture.artifacts.filter((artifact) => artifact.kind === "execution").length, 3);
  assert.equal(fixture.artifacts.filter((artifact) => artifact.kind === "verification").length, 2);
  assert.equal(events.filter((event) => event.type === "agent.waiting_approval").length, 3);
});

test("acceptance fixture: a bounded systemd restart requires approval and verifies state", async () => {
  const fixture = createFixture("task_fixture_service_restart", "service_restart");
  const { events, result } = await runFixture(
    fixture,
    serviceRestartScript(),
    "fixture systemd restart succeeded",
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.equal(fixture.plan?.status, "completed");
  assert.equal(fixture.executions.filter((call) => call.toolName === "systemd.inspect").length, 2);
  assert.equal(fixture.executions.filter((call) => call.toolName === "systemd.restart").length, 1);
  assert.equal(fixture.artifacts.filter((artifact) => artifact.kind === "verification").length, 1);
  assert.equal(events.filter((event) => event.type === "agent.waiting_approval").length, 1);
});

test("acceptance fixture: a bounded package install requires approval and verifies the version", async () => {
  const fixture = createFixture("task_fixture_package_install", "package_install");
  const { events, result } = await runFixture(
    fixture,
    packageInstallScript(),
    "fixture package installation succeeded",
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.equal(fixture.plan?.status, "completed");
  assert.equal(fixture.executions.filter((call) => call.toolName === "package.inspect").length, 2);
  assert.equal(fixture.executions.filter((call) => call.toolName === "package.install").length, 1);
  assert.equal(fixture.artifacts.filter((artifact) => artifact.kind === "verification").length, 1);
  assert.equal(events.filter((event) => event.type === "agent.waiting_approval").length, 1);
});

test("acceptance fixture: backup cleanup lists metadata, requires approval and rechecks the ledger", async () => {
  const fixture = createFixture("task_fixture_backup_cleanup", "backup_cleanup");
  const { events, result } = await runFixture(
    fixture,
    backupCleanupScript(),
    "fixture backup cleanup succeeded",
  );

  assert.equal(result.state, "completed", JSON.stringify(result));
  assert.equal(fixture.plan?.status, "completed");
  assert.equal(fixture.executions.filter((call) => call.toolName === "filesystem.backup.list").length, 2);
  assert.equal(fixture.executions.filter((call) => call.toolName === "filesystem.backup.cleanup").length, 1);
  assert.equal(fixture.backupAvailable, false);
  assert.equal(events.filter((event) => event.type === "agent.waiting_approval").length, 1);
  assert.equal(fixture.artifacts.filter((artifact) => artifact.kind === "verification").length, 1);
});

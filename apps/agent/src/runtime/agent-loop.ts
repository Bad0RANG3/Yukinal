/**
 * Agent Loop — the run is executed here, end to end.
 *
 *   user message
 *     -> policy resolution (the requested policyId, else the environment default)
 *     -> context build
 *     -> provider.stream (real OpenAI-compatible endpoint, ADR 0003/0004 name mapping)
 *     -> tool calls?
 *          no  -> text -> completed
 *          yes -> PermissionEngine.evaluate()      (ADR 0005; one execution authority)
 *               -> auto  -> ToolRegistry.execute(policy_auto / agent_auto / session_auto)
 *               -> ask   -> agent.waiting_approval -> approve? execute(user_approved) : denied result
 *               -> deny  -> denied result to the model
 *          -> results back into messages -> next round (bounded by maxSteps, Stop = abort)
 *     -> report (agent.completed / agent.failed / agent.cancelled)
 *
 * Everything the UI sees is streamed through `hooks.emit` — nothing is buffered
 * until the run is "done", so Stop and trace cards work mid-flight.
 */

import { randomUUID } from "node:crypto";

import {
  RPC_ERROR,
  isSessionGrantable,
  type AgentRunRequest,
  type AgentRunResult,
  type AgentStreamEvent,
  type ApprovalRequest,
  type ApprovalResponse,
  type Evidence,
  type HostEvidenceRecordResponse,
  type HostArtifactRecordRequest,
  type HostArtifactRecordResponse,
  type HostPlanCheckRequest,
  type HostPlanCheckResponse,
  type HostPlanStepResultRequest,
  type HostPlanStepResultResponse,
  type InvestigationArtifact,
  type PermissionDecision,
  type ToolCallRequest,
  type ToolCallResult,
} from "@yukinal/shared";
import { createProviderNameIndex, type LLMProvider, type LlmMessage, type StreamEvent } from "@yukinal/provider-sdk";

import { ContextEngine } from "../context/context-engine.js";
import { buildEvidence } from "../context/evidence.js";
import { PermissionEngine } from "../permissions/permission-engine.js";
import { resolveRequestedPolicy } from "../permissions/policy-registry.js";
import { RpcFailure } from "../errors.js";
import { actionFingerprint } from "../security/action-fingerprint.js";
import { redactSensitiveText, redactSensitiveValue } from "../security/sensitive-data.js";
import { TraceRecorder } from "../trace/trace-recorder.js";
import { ToolRegistry, toolStepTitle, type ExecutionTicket } from "../tools/registry.js";
import {
  agentPromptAudios,
  agentPromptDocuments,
  agentPromptFiles,
  agentPromptImages,
  agentPromptText,
  isRunTimeout,
  positiveInteger,
  promptWithTextFiles,
  runTitle,
  shouldAdvancePlan,
  shouldAutoRecordEvidence,
  shouldCheckPlan,
  summarize,
  waitForObservationSample,
} from "./agent-loop-helpers.js";
import { createToolEventEmitter, serverExecInterruptionState } from "./tool-event-emitter.js";
import { SYSTEM_PROMPT, renderPermissionGuidance, renderRunModeGuidance } from "./prompts.js";

export { shouldAdvancePlan, shouldCheckPlan } from "./agent-loop-helpers.js";

export interface AgentLoopDeps {
  registry: ToolRegistry;
  permission: PermissionEngine;
  context: ContextEngine;
  /** multi-step execution must be bounded. */
  maxSteps?: number;
  /** hard wall-clock bound for one run, including context, provider and tools. */
  maxRunMs?: number;
  /** Approval expiry is injectable so the expiry path can be tested without a two-minute wait. */
  approvalTtlMs?: number;
  /**
   * How the run's `TraceRecorder` is built. Injectable for the same reason
   * `approvalTtlMs` is: the ledger is otherwise unreachable from a test, and "the step
   * of a denied call is closed rather than left running" is a property worth asserting
   * end to end instead of trusting.
   */
  createTrace?: (info: { runId: string; title: string }) => TraceRecorder;
  /** Host-owned persistence for evidence produced by a durable task. */
  recordEvidence?: (evidence: Evidence, signal?: AbortSignal) => Promise<HostEvidenceRecordResponse>;
  /** Host-owned persistence for phase artifacts produced by a durable task. */
  recordArtifact?: (request: HostArtifactRecordRequest, signal?: AbortSignal) => Promise<HostArtifactRecordResponse>;
  /** Host-owned plan gate. It is checked before permission evaluation and execution. */
  checkPlan?: (request: HostPlanCheckRequest, signal?: AbortSignal) => Promise<HostPlanCheckResponse>;
  /** Persist the outcome and advance the active plan step after a call. */
  recordPlanStepResult?: (request: HostPlanStepResultRequest, signal?: AbortSignal) => Promise<HostPlanStepResultResponse>;
}

export interface AgentRunHooks {
  emit(event: AgentStreamEvent): void;
  signal?: AbortSignal;
}

/** Approval 等待器；超时按"已过期"处理（expired → deny）。 */
interface ApprovalWaiter {
  runId: string;
  decision: PermissionDecision;
  resolve(outcome: ApprovalOutcome): void;
}

type ApprovalOutcome = ApprovalResponse["decision"] | "expired";

/**
 * A host-approved continuation for a bounded post-change observation window.
 *
 * The sidecar never invents a new tool or input here. It only remembers the
 * exact final verification read that the model already requested and replays
 * that read after the host-provided sample time. The next plan check and the
 * host's observation clock remain authoritative for every replay.
 */
interface ObservationReplay {
  providerToolName: string;
  input: Record<string, unknown>;
  nextSampleAt?: string;
}

const APPROVAL_TTL_MS = 2 * 60_000;
const DEFAULT_MAX_RUN_MS = 15 * 60_000;
const MAX_RUN_TEXT_CHARS = 200_000;

export class AgentLoop {
  readonly maxSteps: number;
  readonly maxRunMs: number;
  readonly approvalTtlMs: number;
  readonly #approvalWaiters = new Map<string, ApprovalWaiter>();
  readonly #tokensByRun = new Map<string, AbortController>();
  /** Kept only while a run is alive so interrupted terminal events retain their durable task id. */
  readonly #taskIdsByRun = new Map<string, string>();
  /** Per-task limits must survive an abort long enough for the terminal failure to name them. */
  readonly #budgetsByRun = new Map<string, { maxSteps: number; maxRunMs: number }>();

  constructor(readonly deps: AgentLoopDeps) {
    this.maxSteps = positiveInteger(deps.maxSteps ?? 25, "maxSteps");
    this.maxRunMs = positiveInteger(deps.maxRunMs ?? DEFAULT_MAX_RUN_MS, "maxRunMs");
    this.approvalTtlMs = positiveInteger(deps.approvalTtlMs ?? APPROVAL_TTL_MS, "approvalTtlMs");
  }

  get pendingApprovals(): string[] {
    return [...this.#approvalWaiters.keys()];
  }

  /** Stop a run: abort the in-flight request and any pending approval wait. */
  stop(runId: string): boolean {
    const token = this.#tokensByRun.get(runId);
    if (!token) return false;
    token.abort(new Error("cancelled-by-user"));
    return true;
  }

  /** 用户对审批的回应；返回该 approval 是否为本 loop 内挂起的。 */
  respondApproval(response: ApprovalResponse): boolean {
    const waiter = this.#approvalWaiters.get(response.approvalId);
    if (!waiter) return false;
    if (waiter.runId !== response.runId) return false;
    if (response.decision === "approve_session") {
      this.deps.permission.grantSession(waiter.decision);
    }
    waiter.resolve(response.decision);
    return true;
  }

  async start(
    request: AgentRunRequest,
    hooks: AgentRunHooks,
    provider: LLMProvider,
  ): Promise<AgentRunResult> {
    if (!provider) {
      throw new RpcFailure(RPC_ERROR.NOT_IMPLEMENTED, "agent loop requires a configured LLM provider");
    }

    // Resolved before anything else happens in this run: before the run gets a token,
    // before the first `agent.started` is emitted and before the provider is touched.
    // A run that names a policy must either run under *that* policy or not run at all —
    // a fallback to the environment default would silently execute the caller's request
    // under a policy it did not ask for, in whichever direction that environment
    // happens to point.
    const policy = resolveRequestedPolicy(request.policyId);

    const { emit, signal } = hooks;
    const runId = request.runId;
    if (this.#tokensByRun.has(runId)) {
      throw new RpcFailure(RPC_ERROR.INVALID_PARAMS, `runId "${runId}" is already running`);
    }
    const now = (): string => new Date().toISOString();
    const token = new AbortController();
    const onParentAbort = (): void => token.abort(signal?.reason ?? new Error("cancelled-by-parent"));
    signal?.addEventListener("abort", onParentAbort, { once: true });
    if (signal?.aborted) onParentAbort();
    this.#tokensByRun.set(runId, token);
    if (request.taskId) this.#taskIdsByRun.set(runId, request.taskId);
    const maxSteps = Math.min(this.maxSteps, request.taskBudget?.maxSteps ?? this.maxSteps);
    const maxRunMs = Math.min(this.maxRunMs, request.taskBudget?.maxRunMs ?? this.maxRunMs);
    this.#budgetsByRun.set(runId, { maxSteps, maxRunMs });
    const runTimer = setTimeout(() => token.abort(new Error("run-timeout")), maxRunMs);
    runTimer.unref?.();

    let steps = 0;
    let toolCalls = 0;
    let finalText = "";
    let textTruncated = false;
    let inputTokens = 0;
    let outputTokens = 0;

    // One trace per run, and it is the source of both ids every tool event carries:
    // `traceId` names the ledger the audit rows are written against, `stepId` names the
    // card the UI opened. Built here, before the first provider call, because
    // `#finishInterrupted` closes it from every exit path.
    const prompt = agentPromptText(request.parts) || request.prompt.trim();
    const images = agentPromptImages(request.parts);
    const documents = agentPromptDocuments(request.parts);
    const audios = agentPromptAudios(request.parts);
    const files = agentPromptFiles(request.parts);
    const title = runTitle(
      prompt ||
        files[0]?.name ||
        documents[0]?.name ||
        audios[0]?.name ||
        (images.length ? "Image input" : ""),
    );
    const trace = this.deps.createTrace
      ? this.deps.createTrace({ runId, title })
      : new TraceRecorder(runId, title);

    const { emitToolCall, emitToolResult } = createToolEventEmitter({ runId, emit, now });

    try {
      emit({ type: "agent.started", runId, ...(request.taskId ? { taskId: request.taskId } : {}), at: now() });

      const bundle = await this.deps.context.build(
        prompt
          ? request
          : {
              ...request,
              prompt: files.length
                ? "[Text file attachment]"
                : documents.length
                  ? "[PDF document attachment]"
                  : audios.length
                    ? "[Audio attachment]"
                    : "[Image attachment]",
            },
      );
      if (
        !prompt &&
        images.length === 0 &&
        documents.length === 0 &&
        audios.length === 0 &&
        files.length === 0
      ) {
        throw new RpcFailure(
          RPC_ERROR.INVALID_PARAMS,
          "prompt must contain text, an image, a PDF document, a text file, or an audio clip",
        );
      }
      const permissionGuidance = renderPermissionGuidance(request.permissionMode);
      const modeGuidance = renderRunModeGuidance(request.mode);
      const safeContext = redactSensitiveText(bundle.rendered);
      const safePrompt = redactSensitiveText(promptWithTextFiles(prompt, files));
      const messages: LlmMessage[] = [
        {
          role: "system",
          content: safeContext
            ? `${SYSTEM_PROMPT}\n\n${modeGuidance}\n\n${permissionGuidance}\n\n# 上下文\n${safeContext}`
            : `${SYSTEM_PROMPT}\n\n${modeGuidance}\n\n${permissionGuidance}`,
        },
        {
          role: "user",
          content: safePrompt,
          ...(images.length > 0 ? { images } : {}),
          ...(documents.length > 0 ? { documents } : {}),
          ...(audios.length > 0 ? { audios } : {}),
        },
      ];

      const nameIndex = createProviderNameIndex(this.deps.registry.list());
      let observationReplay: ObservationReplay | undefined;

      for (; steps < maxSteps; steps++) {
        if (token.signal.aborted) return this.#finishInterrupted({ runId, steps, toolCalls, text: finalText }, emit, now, token.signal, trace);

        const events: StreamEvent[] = [];
        let streamError: string | null = null;
        for await (const event of provider.stream({
          model: provider.model ?? request.providerConfig?.model ?? "",
          messages,
          tools: nameIndex.specs(),
          signal: token.signal,
          timeoutMs: request.providerConfig?.timeoutMs,
        })) {
          switch (event.type) {
            case "text_delta":
              {
                const safeText = redactSensitiveText(event.text);
                const remaining = MAX_RUN_TEXT_CHARS - finalText.length;
                const delta = remaining > 0 ? safeText.slice(0, remaining) : "";
                finalText += delta;
                if (delta) emit({ type: "agent.text", runId, textDelta: delta, at: now() });
                if (delta.length < safeText.length && !textTruncated) {
                  textTruncated = true;
                  emit({ type: "agent.text", runId, textDelta: "\n\n[输出已截断]", at: now() });
                }
              }
              break;
            case "reasoning_delta":
              {
                // Reasoning is display-only: it may inform the user, but it must never
                // become part of the authoritative answer persisted to chat history.
                const delta = redactSensitiveText(event.text).slice(0, 20_000);
                if (delta) emit({ type: "agent.thinking", runId, textDelta: delta, at: now() });
              }
              break;
            case "usage":
              inputTokens += event.inputTokens;
              outputTokens += event.outputTokens;
              emit({
                type: "agent.usage",
                runId,
                usage: { inputTokens, outputTokens },
                at: now(),
              });
              break;
            case "tool_call":
              events.push(event);
              break;
            case "error":
              streamError = event.message;
              break;
            case "done":
              if (event.finishReason === "cancelled") {
                return this.#finishInterrupted({ runId, steps, toolCalls, text: finalText }, emit, now, token.signal, trace);
              }
              break;
            default:
              break;
          }
        }

        if (token.signal.aborted) return this.#finishInterrupted({ runId, steps, toolCalls, text: finalText }, emit, now, token.signal, trace);
        let calls = events.filter((event): event is Extract<StreamEvent, { type: "tool_call" }> => event.type === "tool_call");
        if (streamError !== null) {
          throw new Error(`provider error: ${streamError}`);
        }
        if (calls.length === 0 && observationReplay !== undefined) {
          const replay = observationReplay;
          const ready = await waitForObservationSample(replay.nextSampleAt ?? "", token.signal);
          if (token.signal.aborted) {
            return this.#finishInterrupted({ runId, steps, toolCalls, text: finalText }, emit, now, token.signal, trace);
          }
          if (!ready) {
            observationReplay = undefined;
            throw new Error("host returned an invalid observation sample schedule");
          }
          observationReplay = undefined;
          calls = [{
            type: "tool_call",
            call: {
              id: `observation_${randomUUID()}`,
              name: replay.providerToolName,
              arguments: replay.input,
            },
          }];
        }
        if (calls.length === 0) break; // 纯文本回合：回答完成

        const traceId = trace.traceId;
        const assistantToolCalls: Array<{ id: string; name: string; arguments: Record<string, unknown> }> = [];
        const toolMessages: LlmMessage[] = [];
        const savePlanResult = async (
          callId: string,
          check: HostPlanCheckResponse | undefined,
          status: "success" | "failed" | "cancelled",
          retryable: boolean,
          outputSummary?: string,
          replay?: ObservationReplay,
        ): Promise<HostPlanStepResultResponse | undefined> => {
          if (!request.taskId || !this.deps.recordPlanStepResult || check?.status !== "allowed") return undefined;
          try {
            const response = await this.deps.recordPlanStepResult(
              {
                taskId: request.taskId,
                planId: check.planId,
                stepId: check.stepId,
                status,
                retryable,
                outputSummary,
              },
              token.signal,
            );
            if (response.recorded && response.observation === "running") {
              observationReplay = response.nextSampleAt && replay
                ? { ...replay, nextSampleAt: response.nextSampleAt }
                : undefined;
              toolMessages.push({
                role: "tool",
                toolCallId: callId,
                content: response.sampleAccepted === false
                  ? `观察窗口仍在进行，尚未到下一次采样时间${response.nextSampleAt ? `（下一次不早于 ${response.nextSampleAt}）` : ""}。本轮不要把任务说成完成。`
                  : `观察窗口已记录一次采样${response.nextSampleAt ? `；下一次不早于 ${response.nextSampleAt}` : ""}，窗口结束前不要把任务说成完成。`,
              });
            } else if (response.recorded && response.observation === "failed") {
              observationReplay = undefined;
              toolMessages.push({
                role: "tool",
                toolCallId: callId,
                content: "观察窗口发现异常，宿主已停在等待用户状态；不要自行扩大操作范围。",
              });
            } else if (response.recorded) {
              // A completed or otherwise terminal plan result closes any pending
              // continuation. The host owns the authoritative window status.
              observationReplay = undefined;
            }
            return response;
          } catch (error) {
            observationReplay = undefined;
            // The tool outcome remains authoritative. A lost progress write is surfaced in
            // the model-visible result so it can stop and ask for a fresh plan instead of
            // assuming the old step advanced.
            toolMessages.push({
              role: "tool",
              toolCallId: callId,
              content: `计划进度未保存：${redactSensitiveText(error instanceof Error ? error.message : String(error))}`,
            });
            return undefined;
          }
        };
        const savePhaseArtifact = async (args: {
          callId: string;
          check: HostPlanCheckResponse | undefined;
          kind: InvestigationArtifact["kind"];
          phase: InvestigationArtifact["phase"];
          status: InvestigationArtifact["status"];
          title: string;
          summary: string;
          content: unknown;
          evidenceIds: string[];
        }): Promise<boolean> => {
          if (!request.taskId || !this.deps.recordArtifact || args.check?.status !== "allowed") return true;
          const timestamp = now();
          const safeSummary = redactSensitiveText(args.summary).slice(0, 8_192);
          const artifact: InvestigationArtifact = {
            id: `artifact_${args.kind}_${runId}_${args.check.stepId}`,
            taskId: request.taskId,
            runId,
            planId: args.check.planId,
            planStepId: args.check.stepId,
            phase: args.phase,
            kind: args.kind,
            status: args.status,
            title: args.title,
            summary: safeSummary,
            content: redactSensitiveValue(args.content),
            evidenceIds: args.evidenceIds.slice(0, 256),
            createdAt: timestamp,
            updatedAt: timestamp,
          };
          try {
            const response = await this.deps.recordArtifact(
              {
                artifact,
                planId: args.check.planId,
                planStepId: args.check.stepId,
                evidenceIds: args.check.evidenceIds,
              },
              token.signal,
            );
            if (response.recorded) return true;
            toolMessages.push({
              role: "tool",
              toolCallId: args.callId,
              content: `阶段工件未保存：${response.error.message}`,
            });
            return false;
          } catch (error) {
            toolMessages.push({
              role: "tool",
              toolCallId: args.callId,
              content: `阶段工件未保存：${redactSensitiveText(error instanceof Error ? error.message : String(error))}`,
            });
            return false;
          }
        };

        for (let index = 0; index < calls.length; index++) {
          if (token.signal.aborted) return this.#finishInterrupted({ runId, steps, toolCalls, text: finalText }, emit, now, token.signal, trace);
          const call = calls[index];
          if (!call) continue;
          const internalName = nameIndex.internalFor(call.call.name);

          if (!internalName) {
            toolMessages.push({ role: "tool", toolCallId: call.call.id, content: `unknown tool: ${call.call.name}` });
            continue;
          }
          const declaration = this.deps.registry.declaration(internalName);
          if (!declaration) {
            toolMessages.push({ role: "tool", toolCallId: call.call.id, content: `unregistered tool: ${internalName}` });
            continue;
          }

          const target = request.target ?? { host: "local" as const, environment: "unknown" as const };

          // A durable task is not allowed to drift silently. Check the host-owned plan
          // before evaluating permission so a call outside the declared step cannot even
          // reach an approval card. Plan and bounded playbook tools are the deliberate
          // exceptions: they create or replace the active revision after a deviation.
          let planCheck: HostPlanCheckResponse | undefined;
          let planCheckError: string | undefined;
          if (
            request.taskId &&
            this.deps.checkPlan &&
            shouldCheckPlan(internalName)
          ) {
            try {
              planCheck = await this.deps.checkPlan(
                { taskId: request.taskId, toolName: internalName, input: call.call.arguments, target },
                token.signal,
              );
            } catch (error) {
              planCheckError = redactSensitiveText(error instanceof Error ? error.message : String(error));
            }
          }

          // Permission decides (ADR 0005). The run mode is an explicit user
          // delegation, not a permission claim embedded in model text.
          const decision = this.deps.permission.evaluate({
            declaration,
            target,
            input: call.call.arguments,
            permissionMode: request.permissionMode,
            mode: request.mode,
            taskId: request.taskId,
            taskCommandGrant: bundle.investigation?.task.guardrails.commandGrant,
            // `undefined` (no policy named in the request) keeps the engine's own
            // environment -> policy default; a named policy was resolved above and is
            // the same one for every call of this run.
            policy,
          });
          // An effectful declaration must never reach an approval card from ordinary chat.
          // The registry and host repeat this check, but denying here keeps the UX honest and
          // avoids asking the user to approve a call that cannot be executed without a plan.
          const durablePlanBound =
            request.taskId !== undefined &&
            planCheck?.status === "allowed" &&
            planCheck.planId.trim().length > 0 &&
            planCheck.stepId.trim().length > 0;
          if (declaration.effectful === true && !durablePlanBound) {
            decision.outcome = "deny";
            decision.approvedBy = undefined;
            decision.approvalId = undefined;
            decision.reason = `${declaration.name} requires a durable task, ChangePlan, and plan step before execution (${decision.reason})`;
          }
          // A plan step that requires approval is satisfied only by a user: a click on this
          // call, or a session grant the user gave for this exact fingerprinted action
          // earlier in the run (ADR 0072). Policy and Agent delegation never satisfy it.
          if (
            planCheck?.status === "allowed" &&
            planCheck.requiresApproval &&
            decision.outcome === "auto" &&
            decision.approvedBy !== "user"
          ) {
            decision.outcome = "ask";
            decision.approvedBy = undefined;
            decision.approvalId = undefined;
            decision.reason = `当前计划步骤要求用户批准：${decision.reason}`;
          }
          assistantToolCalls.push({
            id: call.call.id,
            name: call.call.name,
            arguments: redactSensitiveValue(call.call.arguments) as Record<string, unknown>,
          });
          // The step is opened *before* the card is emitted, because the card carries its
          // id. Every outcome below closes it — including the ones that never reach the
          // registry (denied, rejected, expired), which is where an unclosed step would
          // otherwise sit in the ledger as "running" forever.
          const step = trace.startToolStep({
            title: toolStepTitle(declaration),
            toolName: internalName,
            callInput: call.call.arguments,
            intent: decision.reason,
          });
          const stepId = step.stepId;
          emitToolCall({
            traceId,
            stepId,
            callId: call.call.id,
            toolName: internalName,
            input: call.call.arguments,
            target,
            riskLevel: decision.finalRisk,
            decision: decision.outcome,
            approvedBy: decision.approvedBy,
            policyId: decision.policyId,
            origin: declaration.origin,
            ...(planCheck?.status === "allowed"
              ? {
                  planId: planCheck.planId,
                  planStepId: planCheck.stepId,
                  evidenceIds: planCheck.evidenceIds,
                }
              : {}),
          });

          if (planCheckError !== undefined || (planCheck !== undefined && planCheck.status !== "allowed")) {
            const at = now();
            const deviation = planCheck?.status === "deviation" ? planCheck.deviation : undefined;
            const planError = planCheck?.status === "failed" ? planCheck.error : undefined;
            const errorCode = planCheckError !== undefined ? "internal" : deviation !== undefined ? "plan_deviation" : planError?.code ?? "internal";
            const message = planCheckError ?? deviation?.message ?? planError?.message ?? "计划检查失败";
            toolMessages.push({
              role: "tool",
              toolCallId: call.call.id,
              content: `${planCheckError !== undefined ? "计划检查失败" : "计划未允许此调用"}：${message}`,
            });
            emitToolResult({
              traceId,
              stepId,
              callId: call.call.id,
              toolName: internalName,
              input: call.call.arguments,
              target,
              riskLevel: decision.finalRisk,
              decision: decision.outcome,
              approvedBy: decision.approvedBy,
              policyId: decision.policyId,
              origin: declaration.origin,
              ...(planCheck?.status === "allowed"
                ? {
                    planId: planCheck.planId,
                    planStepId: planCheck.stepId,
                    evidenceIds: planCheck.evidenceIds,
                  }
                : {}),
              status: "failed",
              outputSummary: planCheckError !== undefined ? "计划检查失败" : "计划偏离",
              error: message,
              errorCode,
              startedAt: at,
              endedAt: at,
              durationMs: 0,
            });
            trace.updateStep(stepId, {
              status: "failed",
              kind: "tool",
              outputSummary: planCheckError !== undefined ? "计划检查失败" : "计划偏离",
              error: message,
              endedAt: at,
              durationMs: 0,
            });
            continue;
          }

          let ticket: ExecutionTicket;
          if (decision.outcome === "auto") {
            ticket = decision.approvedBy === "agent"
              ? { kind: "agent_auto", decision }
              : decision.approvedBy === "user"
                ? { kind: "session_auto", decision }
                : { kind: "policy_auto", decision };
          } else if (decision.outcome === "ask") {
            const approvalId = decision.approvalId ?? `apr_${randomUUID()}`;
            decision.approvalId = approvalId;
            const approval: ApprovalRequest = {
              approvalId,
              runId,
              traceId,
              callId: call.call.id,
              inputFingerprint: decision.inputFingerprint ?? actionFingerprint(call.call.arguments),
              toolName: internalName,
              input: redactSensitiveValue(call.call.arguments),
              reason: decision.reason,
              factsSummary: decision.facts.map((fact) => fact.note ?? "").filter(Boolean),
              target,
              ...(planCheck?.status === "allowed"
                ? {
                    planId: planCheck.planId,
                    planStepId: planCheck.stepId,
                    evidenceIds: planCheck.evidenceIds,
                  }
                : {}),
              sessionGrantable: internalName !== "server.exec" && isSessionGrantable(decision),
              expiresAt: new Date(Date.now() + this.approvalTtlMs).toISOString(),
            };
            trace.requireApproval(decision, stepId);
            emit({
              type: "agent.waiting_approval",
              runId,
              ...(request.taskId ? { taskId: request.taskId } : {}),
              approval,
              at: now(),
            });
            const approvalOutcome = await this.#awaitApproval(runId, approval, decision, token);
            if (token.signal.aborted) return this.#finishInterrupted({ runId, steps, toolCalls, text: finalText }, emit, now, token.signal, trace);
            if (approvalOutcome === "reject" || approvalOutcome === "expired") {
              const rejectedAt = now();
              const rejectionSummary = approvalOutcome === "expired" ? "审批已过期" : "权限拒绝";
              if (approvalOutcome === "expired") {
                emit({ type: "agent.approval_expired", runId, approvalId: approval.approvalId, at: rejectedAt });
              }
              toolMessages.push({
                role: "tool",
                toolCallId: call.call.id,
                content: `${rejectionSummary}：${decision.reason}`,
              });
              emitToolResult({
                traceId,
                stepId,
                callId: call.call.id,
                toolName: internalName,
                input: call.call.arguments,
                target,
                riskLevel: decision.finalRisk,
                decision: decision.outcome,
                policyId: decision.policyId,
                origin: declaration.origin,
                ...(planCheck?.status === "allowed"
                  ? {
                      planId: planCheck.planId,
                      planStepId: planCheck.stepId,
                      evidenceIds: planCheck.evidenceIds,
                    }
                  : {}),
                status: "failed",
                outputSummary: rejectionSummary,
                error: approvalOutcome === "expired" ? "approval expired" : decision.reason,
                startedAt: rejectedAt,
                endedAt: rejectedAt,
                durationMs: 0,
              });
              trace.updateStep(stepId, {
                status: "failed",
                kind: "tool",
                outputSummary: rejectionSummary,
                error: approvalOutcome === "expired" ? "approval expired" : decision.reason,
                endedAt: rejectedAt,
                durationMs: 0,
              });
              await savePlanResult(call.call.id, planCheck, "failed", false, rejectionSummary);
              continue;
            }
            ticket = { kind: "user_approved", decision, approvalId: approval.approvalId, respondedAt: now() };
            // Approval turns the step back into an execution in progress; the registry
            // closes it when the call actually finishes.
            trace.updateStep(stepId, { status: "running", kind: "tool" });
          } else {
            const deniedAt = now();
            toolMessages.push({ role: "tool", toolCallId: call.call.id, content: `策略禁止：${decision.reason}` });
            emitToolResult({
              traceId,
              stepId,
              callId: call.call.id,
              toolName: internalName,
              input: call.call.arguments,
              target,
              riskLevel: decision.finalRisk,
              decision: decision.outcome,
              policyId: decision.policyId,
              origin: declaration.origin,
              ...(planCheck?.status === "allowed"
                ? {
                    planId: planCheck.planId,
                    planStepId: planCheck.stepId,
                    evidenceIds: planCheck.evidenceIds,
                  }
                : {}),
              status: "failed",
              outputSummary: "策略禁止",
              error: decision.reason,
              errorCode: "denied_by_policy",
              startedAt: deniedAt,
              endedAt: deniedAt,
              durationMs: 0,
            });
            trace.updateStep(stepId, {
              status: "failed",
              kind: "tool",
              outputSummary: "策略禁止",
              error: decision.reason,
              endedAt: deniedAt,
              durationMs: 0,
            });
            await savePlanResult(call.call.id, planCheck, "failed", false, "策略禁止");
            continue;
          }

          toolCalls += 1;
          const startedAt = now();
          const result = await this.deps.registry.execute(
            {
              callId: call.call.id,
              runId,
              traceId,
              toolName: internalName,
              input: call.call.arguments,
              target,
              taskId: request.taskId,
              ...(planCheck?.status === "allowed"
                ? {
                    planId: planCheck.planId,
                    planStepId: planCheck.stepId,
                    evidenceIds: planCheck.evidenceIds,
                  }
                : {}),
              intent: decision.reason,
            } satisfies ToolCallRequest,
            ticket,
            {
              signal: token.signal,
              trace,
              stepId,
              permissionMode: request.permissionMode,
              mode: request.mode ?? "goal",
            },
          );
          if (token.signal.aborted) return this.#finishInterrupted({ runId, steps, toolCalls, text: finalText }, emit, now, token.signal, trace);
          const toolMessageIndex = toolMessages.length;
          this.#consumeResult(result, (output) =>
            toolMessages.push({ role: "tool", toolCallId: call.call.id, content: output }),
          );
          const appendToToolMessage = (content: string): void => {
            const existing = toolMessages[toolMessageIndex];
            if (existing?.role === "tool") {
              existing.content = `${existing.content}\n${content}`;
            } else {
              toolMessages.push({ role: "tool", toolCallId: call.call.id, content });
            }
          };
          let planResultStatus: "success" | "failed" | "cancelled" =
            result.status === "success" ? "success" : result.status === "cancelled" ? "cancelled" : "failed";
          let planResultRetryable = result.error?.retryable ?? false;
          let planResultOutputSummary = result.outputSummary;
          if (
            request.taskId &&
            result.status === "success" &&
            decision.finalRisk === "read" &&
            shouldAutoRecordEvidence(internalName) &&
            this.deps.recordEvidence
          ) {
            try {
              const evidence = buildEvidence({
                taskId: request.taskId,
                target,
                toolName: internalName,
                input: call.call.arguments,
                output: result.output,
                collectedAt: result.endedAt,
              });
              const recorded = await this.deps.recordEvidence(evidence, token.signal);
              if (!recorded.recorded) {
                appendToToolMessage(`证据未保存：${recorded.error.message}`);
                planResultStatus = "failed";
                planResultRetryable = recorded.error.retryable;
                planResultOutputSummary = `证据未保存：${recorded.error.message}`;
              } else {
                const evidenceId = recorded.evidenceId ?? evidence.id;
                const reused = recorded.reused === true;
                const reuseNote = reused ? "（本次运行已复用相同证据）" : "";
                // The evidence id is the durable join key for findings, briefs and later
                // recovery. Return it in the model-visible tool message; a boolean
                // `recorded: true` is not enough for the next turn to cite the evidence
                // without guessing an id that the host will (correctly) reject.
                appendToToolMessage(`证据已保存：${evidenceId}${reuseNote}`);
                if (reused && planCheck?.status === "allowed" && planCheck.stepKind === "evidence") {
                  // A persistence dedupe is not a fresh observation. Treating it as a
                  // successful evidence step would let a durable plan advance forever
                  // while the target keeps returning the same sample. The host records
                  // this as a non-retryable plan failure, which leaves the user with a
                  // truthful failure artifact and forces a re-plan or an explicit wait.
                  planResultStatus = "failed";
                  planResultRetryable = false;
                  planResultOutputSummary = "本次读取没有产生新的调查证据，计划步骤未推进；需要重规划或等待用户决定";
                  appendToToolMessage(`${planResultOutputSummary}。请改用证据检索/比较/关联，或调整采样条件后重新规划。`);
                } else if (planCheck?.status === "allowed" && planCheck.stepKind === "evidence") {
                  const saved = await savePhaseArtifact({
                    callId: call.call.id,
                    check: planCheck,
                    kind: "baseline",
                    phase: "investigating",
                    status: "ready",
                    title: "自动记录的调查基线",
                    summary: `来自 ${internalName} 的成功只读结果`,
                    content: {
                      sourceTool: internalName,
                      evidenceId,
                      reused,
                      outputSummary: result.outputSummary ?? summarize(result.output),
                    },
                    evidenceIds: [evidenceId],
                  });
                  if (!saved) {
                    planResultStatus = "failed";
                    planResultRetryable = false;
                  }
                }
              }
            } catch (error) {
              const message = redactSensitiveText(error instanceof Error ? error.message : String(error));
              appendToToolMessage(`证据未保存：${message}`);
              planResultStatus = "failed";
              planResultRetryable = false;
              planResultOutputSummary = `证据未保存：${message}`;
            }
          }
          if (request.taskId && result.status === "success" && planCheck?.status === "allowed") {
            const automaticArtifact =
              planCheck.stepKind === "action"
                ? {
                    kind: "execution" as const,
                    phase: "execution" as const,
                    status: "succeeded" as const,
                    title: "自动记录的执行结果",
                    summary: result.outputSummary ?? summarize(result.output),
                    content: {
                      tool: internalName,
                      status: result.status,
                      outputSummary: result.outputSummary ?? summarize(result.output),
                    },
                  }
                : planCheck.stepKind === "verification"
                  ? {
                      kind: "verification" as const,
                      phase: "verification" as const,
                      status: "succeeded" as const,
                      title: "自动记录的验证结果",
                      summary: result.outputSummary ?? summarize(result.output),
                      content: {
                        tool: internalName,
                        status: result.status,
                        outputSummary: result.outputSummary ?? summarize(result.output),
                      },
                    }
                  : undefined;
            if (automaticArtifact) {
              const saved = await savePhaseArtifact({
                callId: call.call.id,
                check: planCheck,
                ...automaticArtifact,
                evidenceIds: planCheck.evidenceIds,
              });
              if (!saved) {
                planResultStatus = "failed";
                planResultRetryable = false;
              }
            }
          }
          if (shouldAdvancePlan(internalName)) {
            await savePlanResult(
              call.call.id,
              planCheck,
              planResultStatus,
              planResultRetryable,
              planResultOutputSummary,
              result.status === "success" && planCheck?.status === "allowed" && planCheck.stepKind === "verification"
                ? {
                    providerToolName: call.call.name,
                    input: { ...call.call.arguments },
                  }
                : undefined,
            );
          }
          const executionState = serverExecInterruptionState(internalName, result.error?.detail);
          emitToolResult({
            traceId,
            stepId,
            callId: call.call.id,
            toolName: internalName,
            input: redactSensitiveValue(call.call.arguments),
            target,
            riskLevel: decision.finalRisk,
            decision: decision.outcome,
            approvedBy: ticket.kind === "policy_auto" ? "policy" : ticket.kind === "agent_auto" ? "agent" : "user",
            policyId: decision.policyId,
            origin: declaration.origin,
            ...(planCheck?.status === "allowed"
              ? {
                  planId: planCheck.planId,
                  planStepId: planCheck.stepId,
                  evidenceIds: planCheck.evidenceIds,
                }
              : {}),
            status: result.status === "success" ? "success" : result.status === "cancelled" ? "cancelled" : "failed",
            outputSummary: redactSensitiveText(result.outputSummary ?? summarize(result.output)),
            error: result.error?.message === undefined ? undefined : redactSensitiveText(result.error.message),
            errorCode: result.error?.code,
            ...(executionState ? { executionState } : {}),
            startedAt: result.startedAt || startedAt,
            endedAt: result.endedAt,
            durationMs: result.durationMs,
          });
        }

        // 把这一轮的 assistant tool calls + 结果回灌给模型，进入下一轮。
        messages.push({ role: "assistant", content: "", toolCalls: assistantToolCalls });
        messages.push(...toolMessages);
      }

      if (token.signal.aborted) return this.#finishInterrupted({ runId, steps, toolCalls, text: finalText }, emit, now, token.signal, trace);
      if (steps >= maxSteps && finalText.trim().length === 0) {
        throw new RpcFailure(RPC_ERROR.TIMEOUT, `run exceeded maxSteps=${maxSteps}`);
      }

      finalText = finalText.trim();
      const result: AgentRunResult = { runId, state: "completed", text: finalText, steps, toolCalls, traceId: trace.traceId };
      emit({ type: "agent.completed", runId, ...(request.taskId ? { taskId: request.taskId } : {}), result, at: now() });
      trace.finish("completed");
      return result;
    } catch (error) {
      if (token.signal.aborted) return this.#finishInterrupted({ runId, steps, toolCalls, text: finalText }, emit, now, token.signal, trace);
      const message = redactSensitiveText(error instanceof Error ? error.message : String(error));
      const result: AgentRunResult = { runId, state: "failed", text: finalText.trim(), steps, toolCalls, error: message, traceId: trace.traceId };
      emit({ type: "agent.failed", runId, ...(request.taskId ? { taskId: request.taskId } : {}), error: message, at: now() });
      trace.finish("failed");
      return result;
    } finally {
      clearTimeout(runTimer);
      signal?.removeEventListener("abort", onParentAbort);
      this.#tokensByRun.delete(runId);
      this.#taskIdsByRun.delete(runId);
      this.#budgetsByRun.delete(runId);
      for (const [approvalId, waiter] of this.#approvalWaiters) {
        if (waiter.runId === runId) {
          this.#approvalWaiters.delete(approvalId);
          waiter.resolve("reject");
        }
      }
      // "Approve for this run" has to end with the run. The PermissionEngine
      // outlives individual runs, so without this the grant set would keep
      // authorising later, unrelated runs for the life of the sidecar process.
      // Clearing only once nothing else is in flight keeps a concurrent run's
      // own grants intact.
      if (this.#tokensByRun.size === 0) this.deps.permission.clearGrants();
    }
  }

  #consumeResult(result: ToolCallResult, pushToolMessage: (summary: string) => void): void {
    if (result.status === "success") {
      pushToolMessage(redactSensitiveText(result.outputSummary ?? (result.output !== undefined ? JSON.stringify(result.output) : "(no output)")));
      return;
    }
    if (result.status === "cancelled") {
      pushToolMessage("(cancelled)");
      return;
    }
    const detail = result.error?.detail === undefined ? "" : `\n受限结果：${redactSensitiveText(JSON.stringify(result.error.detail)).slice(0, 4_000)}`;
    pushToolMessage(`工具失败：${redactSensitiveText(result.error?.message ?? "unknown")}（code ${result.error?.code ?? "?"}）${detail}`);
  }

  #finishInterrupted(
    info: { runId: string; steps: number; toolCalls: number; text: string },
    emit: (event: AgentStreamEvent) => void,
    now: () => string,
    signal: AbortSignal,
    trace: TraceRecorder,
  ): AgentRunResult {
    if (isRunTimeout(signal)) {
      const error = `run exceeded maxRunMs=${this.#budgetsByRun.get(info.runId)?.maxRunMs ?? this.maxRunMs}`;
      const result: AgentRunResult = {
        runId: info.runId,
        state: "failed",
        text: info.text.trim(),
        steps: info.steps,
        toolCalls: info.toolCalls,
        error,
        traceId: trace.traceId,
      };
      const taskId = this.#taskIdsByRun.get(info.runId);
      emit({ type: "agent.failed", runId: info.runId, ...(taskId ? { taskId } : {}), error, at: now() });
      trace.finish("failed");
      return result;
    }
    const result: AgentRunResult = {
      runId: info.runId,
      state: "cancelled",
      text: info.text.trim(),
      steps: info.steps,
      toolCalls: info.toolCalls,
      traceId: trace.traceId,
    };
    emit({ type: "agent.text", runId: info.runId, textDelta: "\n\n[已停止]", at: now() });
    const taskId = this.#taskIdsByRun.get(info.runId);
    emit({ type: "agent.completed", runId: info.runId, ...(taskId ? { taskId } : {}), result, at: now() });
    trace.finish("cancelled");
    return result;
  }

  #awaitApproval(
    runId: string,
    approval: ApprovalRequest,
    decision: PermissionDecision,
    token: AbortController,
  ): Promise<ApprovalOutcome> {
    return new Promise<ApprovalOutcome>((resolve) => {
      let settled = false;
      let timer: ReturnType<typeof setTimeout> | undefined;
      const onAbort = (): void => finish("reject");
      const finish = (outcome: ApprovalOutcome): void => {
        if (settled) return;
        settled = true;
        if (timer !== undefined) clearTimeout(timer);
        token.signal.removeEventListener("abort", onAbort);
        this.#approvalWaiters.delete(approval.approvalId);
        resolve(outcome);
      };
      this.#approvalWaiters.set(approval.approvalId, { runId, decision, resolve: finish });
      // TTL：过期按拒绝处理，避免 run 永久挂起（approval_expired 语义）。
      timer = setTimeout(() => finish("expired"), this.approvalTtlMs);
      // 用户 Stop 也要解开等待。
      if (token.signal.aborted) finish("reject");
      else token.signal.addEventListener("abort", onAbort, { once: true });
    });
  }
}

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
  type AgentRunRequest,
  type AgentRunResult,
  type AgentStreamEvent,
  type ApprovalRequest,
  type ApprovalResponse,
  type PermissionApprovalSource,
  type PermissionDecision,
  type ToolCallRequest,
  type ToolCallResult,
  type ToolDeclaration,
} from "@yukinal/shared";
import { createProviderNameIndex, type LLMProvider, type LlmMessage, type StreamEvent } from "@yukinal/provider-sdk";

import { ContextEngine } from "../context/context-engine.js";
import { PermissionEngine } from "../permissions/permission-engine.js";
import { resolveRequestedPolicy } from "../permissions/policy-registry.js";
import { RpcFailure } from "../errors.js";
import { redactSensitiveText, redactSensitiveValue } from "../security/sensitive-data.js";
import { TraceRecorder } from "../trace/trace-recorder.js";
import { ToolRegistry, toolStepTitle, type ExecutionTicket } from "../tools/registry.js";
import { SYSTEM_PROMPT, renderPermissionGuidance, renderRunModeGuidance } from "./prompts.js";

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

const APPROVAL_TTL_MS = 2 * 60_000;
const DEFAULT_MAX_RUN_MS = 15 * 60_000;
const MAX_RUN_TEXT_CHARS = 200_000;

export class AgentLoop {
  readonly maxSteps: number;
  readonly maxRunMs: number;
  readonly approvalTtlMs: number;
  readonly #approvalWaiters = new Map<string, ApprovalWaiter>();
  readonly #tokensByRun = new Map<string, AbortController>();

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
    const runTimer = setTimeout(() => token.abort(new Error("run-timeout")), this.maxRunMs);
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
    const prompt = request.parts?.map((part) => part.text).join("\n").trim() || request.prompt.trim();
    const title = runTitle(prompt);
    const trace = this.deps.createTrace
      ? this.deps.createTrace({ runId, title })
      : new TraceRecorder(runId, title);

    const emitToolCall = (call: {
      traceId: string;
      stepId: string;
      callId: string;
      toolName: string;
      input: unknown;
      target: ToolCallRequest["target"];
      riskLevel: PermissionDecision["finalRisk"];
      decision: PermissionDecision["outcome"];
      approvedBy?: PermissionApprovalSource;
      /**
       * The policy the decision was made under, taken from the decision itself rather
       * than from the request: the engine is the authority on which policy it applied,
       * and this is the field that makes that observable from the event stream.
       */
      policyId: PermissionDecision["policyId"];
      /**
       * Where the tool came from. Carried on the event so the audit trail and the UI can tell
       * a third-party (MCP) tool from a built-in one without inferring it from the name.
       */
      origin: ToolDeclaration["origin"];
    }): void => {
      emit({
        type: "agent.tool_call",
        runId,
        traceId: call.traceId,
        stepId: call.stepId,
        callId: call.callId,
        toolName: call.toolName,
        input: redactSensitiveValue(call.input),
        target: call.target,
        riskLevel: call.riskLevel,
        decision: call.decision,
        approvedBy: call.approvedBy,
        policyId: call.policyId,
        origin: call.origin,
        at: now(),
      });
    };

    const emitToolResult = (result: {
      traceId: string;
      stepId: string;
      callId: string;
      toolName: string;
      input: unknown;
      target: ToolCallRequest["target"];
      riskLevel: PermissionDecision["finalRisk"];
      decision: PermissionDecision["outcome"];
      approvedBy?: PermissionApprovalSource;
      policyId: PermissionDecision["policyId"];
      /** See the note on the same field in `emitToolCall`. */
      origin: ToolDeclaration["origin"];
      status: "success" | "failed" | "cancelled";
      outputSummary: string;
      error?: string;
      startedAt: string;
      endedAt: string;
      durationMs: number;
    }): void => {
      emit({
        type: "agent.tool_result",
        runId,
        traceId: result.traceId,
        stepId: result.stepId,
        callId: result.callId,
        toolName: result.toolName,
        input: redactSensitiveValue(result.input),
        target: result.target,
        riskLevel: result.riskLevel,
        decision: result.decision,
        approvedBy: result.approvedBy,
        policyId: result.policyId,
        origin: result.origin,
        status: result.status,
        outputSummary: redactSensitiveText(result.outputSummary),
        error: result.error === undefined ? undefined : redactSensitiveText(result.error),
        startedAt: result.startedAt,
        endedAt: result.endedAt,
        durationMs: result.durationMs,
        at: result.endedAt,
      });
    };

    try {
      emit({ type: "agent.started", runId, at: now() });

      const bundle = await this.deps.context.build(request);
      if (!prompt) throw new RpcFailure(RPC_ERROR.INVALID_PARAMS, "prompt must not be blank");
      const permissionGuidance = renderPermissionGuidance(request.permissionMode);
      const modeGuidance = renderRunModeGuidance(request.mode);
      const safeContext = redactSensitiveText(bundle.rendered);
      const safePrompt = redactSensitiveText(prompt);
      const messages: LlmMessage[] = [
        {
          role: "system",
          content: safeContext
            ? `${SYSTEM_PROMPT}\n\n${modeGuidance}\n\n${permissionGuidance}\n\n# 上下文\n${safeContext}`
            : `${SYSTEM_PROMPT}\n\n${modeGuidance}\n\n${permissionGuidance}`,
        },
        { role: "user", content: safePrompt },
      ];

      const nameIndex = createProviderNameIndex(this.deps.registry.list());

      for (; steps < this.maxSteps; steps++) {
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
        const calls = events.filter((event): event is Extract<StreamEvent, { type: "tool_call" }> => event.type === "tool_call");
        if (streamError !== null) {
          throw new Error(`provider error: ${streamError}`);
        }
        if (calls.length === 0) break; // 纯文本回合：回答完成

        const traceId = trace.traceId;
        const assistantToolCalls: Array<{ id: string; name: string; arguments: Record<string, unknown> }> = [];
        const toolMessages: LlmMessage[] = [];

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

          // Permission decides (ADR 0005). The run mode is an explicit user
          // delegation, not a permission claim embedded in model text.
          const target = request.target ?? { host: "local" as const, environment: "unknown" as const };
          const decision = this.deps.permission.evaluate({
            declaration,
            target,
            input: call.call.arguments,
            permissionMode: request.permissionMode,
            mode: request.mode,
            // `undefined` (no policy named in the request) keeps the engine's own
            // environment -> policy default; a named policy was resolved above and is
            // the same one for every call of this run.
            policy,
          });
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
          });

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
              toolName: internalName,
              input: redactSensitiveValue(call.call.arguments),
              reason: decision.reason,
              factsSummary: decision.facts.map((fact) => fact.note ?? "").filter(Boolean),
              target,
              expiresAt: new Date(Date.now() + this.approvalTtlMs).toISOString(),
            };
            trace.requireApproval(decision, stepId);
            emit({ type: "agent.waiting_approval", runId, approval, at: now() });
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
              status: "failed",
              outputSummary: "策略禁止",
              error: decision.reason,
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
            continue;
          }

          toolCalls += 1;
          const startedAt = now();
          const result = await this.deps.registry.execute(
            {
              callId: call.call.id,
              traceId,
              toolName: internalName,
              input: call.call.arguments,
              target,
              intent: decision.reason,
            } satisfies ToolCallRequest,
            ticket,
            { signal: token.signal, trace, stepId },
          );
          if (token.signal.aborted) return this.#finishInterrupted({ runId, steps, toolCalls, text: finalText }, emit, now, token.signal, trace);
          this.#consumeResult(result, (output) =>
            toolMessages.push({ role: "tool", toolCallId: call.call.id, content: output }),
          );
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
            status: result.status === "success" ? "success" : result.status === "cancelled" ? "cancelled" : "failed",
            outputSummary: redactSensitiveText(result.outputSummary ?? summarize(result.output)),
            error: result.error?.message === undefined ? undefined : redactSensitiveText(result.error.message),
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
      if (steps >= this.maxSteps && finalText.trim().length === 0) {
        throw new RpcFailure(RPC_ERROR.TIMEOUT, `run exceeded maxSteps=${this.maxSteps}`);
      }

      finalText = finalText.trim();
      const result: AgentRunResult = { runId, state: "completed", text: finalText, steps, toolCalls, traceId: trace.traceId };
      emit({ type: "agent.completed", runId, result, at: now() });
      trace.finish("completed");
      return result;
    } catch (error) {
      if (token.signal.aborted) return this.#finishInterrupted({ runId, steps, toolCalls, text: finalText }, emit, now, token.signal, trace);
      const message = redactSensitiveText(error instanceof Error ? error.message : String(error));
      const result: AgentRunResult = { runId, state: "failed", text: finalText.trim(), steps, toolCalls, error: message, traceId: trace.traceId };
      emit({ type: "agent.failed", runId, error: message, at: now() });
      trace.finish("failed");
      return result;
    } finally {
      clearTimeout(runTimer);
      signal?.removeEventListener("abort", onParentAbort);
      this.#tokensByRun.delete(runId);
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
    pushToolMessage(`工具失败：${redactSensitiveText(result.error?.message ?? "unknown")}（code ${result.error?.code ?? "?"}）`);
  }

  #finishInterrupted(
    info: { runId: string; steps: number; toolCalls: number; text: string },
    emit: (event: AgentStreamEvent) => void,
    now: () => string,
    signal: AbortSignal,
    trace: TraceRecorder,
  ): AgentRunResult {
    if (isRunTimeout(signal)) {
      const error = `run exceeded maxRunMs=${this.maxRunMs}`;
      const result: AgentRunResult = {
        runId: info.runId,
        state: "failed",
        text: info.text.trim(),
        steps: info.steps,
        toolCalls: info.toolCalls,
        error,
        traceId: trace.traceId,
      };
      emit({ type: "agent.failed", runId: info.runId, error, at: now() });
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
    emit({ type: "agent.completed", runId: info.runId, result, at: now() });
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

function positiveInteger(value: number, name: string): number {
  if (!Number.isInteger(value) || value <= 0) throw new Error(`${name} must be a positive integer`);
  return value;
}

/**
 * The trace ledger's title for one run: the first non-blank line of the user's prompt,
 * redacted and bounded.
 *
 * Taken from the prompt rather than from the model's answer because the title exists to
 * make a **finished** run recognisable in a list of traces — and a run that failed before
 * it produced any text is exactly the one a reader is looking for.
 */
function runTitle(prompt: string): string {
  const firstLine =
    redactSensitiveText(prompt)
      .split("\n")
      .map((line) => line.trim())
      .find((line) => line.length > 0) ?? "";
  if (!firstLine) return "Agent run";
  return firstLine.length > 80 ? `${firstLine.slice(0, 80)}…` : firstLine;
}

function isRunTimeout(signal: AbortSignal): boolean {
  const reason = signal.reason;
  return reason instanceof Error && reason.message === "run-timeout";
}

function summarize(output: unknown): string {
  if (output === undefined || output === null) return "(no output)";
  const text = redactSensitiveText(typeof output === "string" ? output : JSON.stringify(output));
  return text.length > 400 ? `${text.slice(0, 400)}…` : text;
}

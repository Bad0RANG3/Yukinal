/**
 * Derive bounded, non-causal diagnostic signals from already persisted evidence.
 *
 * This deliberately runs after the host has redacted and task-scoped each evidence
 * item.  It is a convenience for the Agent's read-only reasoning loop, not a second
 * source of truth: it never samples a remote target, writes an evidence row, advances
 * a plan, or turns a signal into an authorization.
 */

import { EvidenceSchema } from "@yukinal/shared";
import type { Evidence } from "@yukinal/shared";
import { z } from "zod";

import { ToolFailure, type Tool } from "../tool.js";
import type { HostRpcClient } from "../../transport/host-client.js";

const MAX_EVIDENCE = 16;
const MAX_SIGNALS = 64;
const MAX_HYPOTHESES = 8;
const MAX_WARNINGS = 16;

const input = z
  .strictObject({
    evidenceIds: z.array(z.string().trim().min(1).max(256)).min(1).max(MAX_EVIDENCE),
    focus: z.string().trim().min(1).max(160).optional(),
  })
  .superRefine((request, context) => {
    if (new Set(request.evidenceIds).size !== request.evidenceIds.length) {
      context.addIssue({ code: z.ZodIssueCode.custom, path: ["evidenceIds"], message: "evidenceIds must be unique" });
    }
  });

const TRIAGE_SEVERITIES = ["info", "warning", "critical"] as const;
const TRIAGE_CONFIDENCES = ["low", "medium"] as const;

const signal = z.strictObject({
  evidenceId: z.string().min(1).max(256),
  sourceTool: z.string().min(1).max(160),
  code: z.string().regex(/^[a-z][a-z0-9_]{1,63}$/),
  severity: z.enum(TRIAGE_SEVERITIES),
  confidence: z.enum(TRIAGE_CONFIDENCES),
  summary: z.string().min(1).max(512),
  count: z.number().int().nonnegative().optional(),
});

const hypothesis = z.strictObject({
  code: z.string().regex(/^[a-z][a-z0-9_]{1,63}$/),
  title: z.string().min(1).max(160),
  statement: z.string().min(1).max(768),
  evidenceIds: z.array(z.string().min(1).max(256)).max(MAX_EVIDENCE),
  signalCodes: z.array(z.string().regex(/^[a-z][a-z0-9_]{1,63}$/)).max(16),
  confidence: z.enum(TRIAGE_CONFIDENCES),
  nextVerification: z.string().min(1).max(512),
});

const output = z.strictObject({
  evidenceIds: z.array(z.string().min(1).max(256)).max(MAX_EVIDENCE),
  evaluatedEvidence: z.number().int().nonnegative().max(MAX_EVIDENCE),
  signals: z.array(signal).max(MAX_SIGNALS),
  hypotheses: z.array(hypothesis).max(MAX_HYPOTHESES),
  warnings: z.array(z.string().min(1).max(512)).max(MAX_WARNINGS),
  /** A hard reminder for both the model and tests that this is not causal inference. */
  notCausal: z.literal(true),
});

type TriageInput = z.infer<typeof input>;
type TriageOutput = z.infer<typeof output>;
type TriageSignal = z.infer<typeof signal>;

type EvidenceFetcher = Pick<HostRpcClient, "fetchEvidence">;

const textPatternRules = [
  { code: "out_of_memory", pattern: /\b(?:oom|out of memory|memory pressure)\b/i, label: "内存不足或 OOM" },
  { code: "timeout", pattern: /\b(?:timeout|timed out|deadline exceeded)\b/i, label: "超时" },
  { code: "connection_refused", pattern: /\b(?:connection refused|connect(?:ion)? reset|econnrefused)\b/i, label: "连接被拒绝或重置" },
  { code: "panic_or_exception", pattern: /\b(?:panic|exception|traceback|segmentation fault)\b/i, label: "异常、panic 或崩溃" },
] as const;

export function investigationEvidenceTriageTool(
  host: EvidenceFetcher,
): Tool<TriageInput, TriageOutput> {
  return {
    name: "investigation.evidence.triage",
    description:
      "Analyze up to 16 already persisted and host-redacted evidence items with bounded " +
      "read-only heuristics. It returns warning signals and low-confidence candidate hypotheses, " +
      "never raw bodies, new samples, causal proof, plan progress or write authorization.",
    risk: "read",
    timeoutMs: 15_000,
    cancellable: true,
    retry: { maxAttempts: 1, backoffMs: 0 },
    input,
    async execute(request, context) {
      if (!context.taskId) {
        throw new ToolFailure("investigation.evidence.triage requires a durable task", "invalid_input", false);
      }

      const fetched = await Promise.all(
        request.evidenceIds.map(async (evidenceId) => {
          const response = await host.fetchEvidence({ taskId: context.taskId!, evidenceId }, context.signal);
          if (response.status === "success") return { evidenceId, evidence: EvidenceSchema.parse(response.evidence) };
          if (response.status === "not_found") return { evidenceId, error: "evidence not found in the current task" };
          return { evidenceId, error: response.error.message };
        }),
      );
      const evidence = fetched.flatMap((item) => ("evidence" in item && item.evidence ? [item.evidence] : []));
      if (evidence.length === 0) {
        throw new ToolFailure(
          "none of the requested evidence items could be retrieved from the current task",
          "not_found",
          false,
          fetched.map((item) => `${item.evidenceId}: ${item.error ?? "unavailable"}`).join("; "),
        );
      }

      const result = triageEvidence(evidence, fetched, request.focus);
      return output.parse(result);
    },
  };
}

function triageEvidence(
  evidence: Evidence[],
  fetched: Array<{ evidenceId: string; evidence: Evidence } | { evidenceId: string; error: string }>,
  focus?: string,
): TriageOutput {
  const signals: TriageSignal[] = [];
  const warnings: string[] = [];
  const addSignal = (candidate: TriageSignal): void => {
    if (signals.length < MAX_SIGNALS) signals.push(candidate);
  };
  const addWarning = (message: string): void => {
    if (!warnings.includes(message) && warnings.length < MAX_WARNINGS) warnings.push(message);
  };

  for (const item of fetched) {
    if ("error" in item) {
      addWarning(`${item.evidenceId}: ${boundText(item.error, 480)}`);
      continue;
    }
    const evidenceItem = item.evidence;
    const freshness = evidenceItem.freshness?.status;
    if (evidenceItem.truncated) {
      addSignal({
        evidenceId: evidenceItem.id,
        sourceTool: evidenceItem.sourceTool,
        code: "evidence_truncated",
        severity: "warning",
        confidence: "medium",
        summary: "证据被宿主截断，不能据此排除未显示的异常。",
      });
    }
    if (freshness && freshness !== "fresh") {
      addWarning(`${evidenceItem.id}: 证据新鲜度为 ${freshness}，只能作为历史上下文。`);
    }
    inspectEvidence(evidenceItem, addSignal);
  }

  if (signals.length === 0) {
    addWarning(focus ? `未在“${boundText(focus, 120)}”范围内命中已知信号；这不等于目标健康。` : "未命中已知信号；这不等于目标健康。");
  }

  return {
    evidenceIds: evidence.map((item) => item.id),
    evaluatedEvidence: evidence.length,
    signals,
    hypotheses: buildHypotheses(signals, focus),
    warnings,
    notCausal: true,
  };
}

function inspectEvidence(evidence: Evidence, addSignal: (signal: TriageSignal) => void): void {
  const content = asRecord(evidence.content);
  if (evidence.sourceTool === "server.info") {
    const health = stringValue(content.health);
    if (health && !["healthy", "ok", "information"].includes(health.toLowerCase())) {
      addSignal({
        evidenceId: evidence.id,
        sourceTool: evidence.sourceTool,
        code: "server_health_non_healthy",
        severity: health.toLowerCase() === "critical" ? "critical" : "warning",
        confidence: "medium",
        summary: `服务器快照报告 health=${boundText(health, 80)}，需要结合其它证据核对。`,
      });
    }
    const resourceFields = [
      ["cpu", asRecord(content.cpu).usagePercent],
      ["memory", asRecord(content.memory).usagePercent],
    ] as const;
    const stressed = resourceFields.filter(([, value]) => typeof value === "number" && value >= 90);
    if (stressed.length > 0) {
      addSignal({
        evidenceId: evidence.id,
        sourceTool: evidence.sourceTool,
        code: "resource_pressure",
        severity: "critical",
        confidence: "medium",
        summary: `服务器快照中 ${stressed.map(([name]) => name).join("、")} 使用率达到高水位。`,
        count: stressed.length,
      });
    }
    const collectors = Array.isArray(content.collectors) ? content.collectors : [];
    const failedCollectors = collectors.filter((entry) => asRecord(entry).ok === false).length;
    if (failedCollectors > 0) {
      addSignal({
        evidenceId: evidence.id,
        sourceTool: evidence.sourceTool,
        code: "collector_unavailable",
        severity: "warning",
        confidence: "medium",
        summary: `${failedCollectors} 个健康采集器返回失败，快照可能不完整。`,
        count: failedCollectors,
      });
    }
  }

  if (evidence.sourceTool === "server.logs" || evidence.sourceTool === "docker.logs") {
    const lines = Array.isArray(content.lines) ? content.lines : [];
    const texts = lines.map((line) => (typeof line === "string" ? line : stringValue(asRecord(line).text) ?? ""));
    const levelErrors = lines.filter((line) => ["error", "warning"].includes(stringValue(asRecord(line).level)?.toLowerCase() ?? "")).length;
    if (levelErrors > 0) {
      addSignal({
        evidenceId: evidence.id,
        sourceTool: evidence.sourceTool,
        code: "log_error_level",
        severity: "warning",
        confidence: "low",
        summary: `日志摘要包含 ${levelErrors} 条 error/warning 级别记录。`,
        count: levelErrors,
      });
    }
    for (const rule of textPatternRules) {
      const count = texts.filter((text) => rule.pattern.test(text)).length;
      if (count > 0) {
        addSignal({
          evidenceId: evidence.id,
          sourceTool: evidence.sourceTool,
          code: `log_${rule.code}`,
          severity: rule.code === "out_of_memory" || rule.code === "panic_or_exception" ? "critical" : "warning",
          confidence: "low",
          summary: `日志中命中“${rule.label}”模式 ${count} 次；仅表示文本信号，不代表根因。`,
          count,
        });
      }
    }
  }

  if (evidence.sourceTool === "server.services") {
    const services = Array.isArray(content.services) ? content.services : [];
    const degraded = services.filter((service) => ["stopped", "failed"].includes(stringValue(asRecord(service).state)?.toLowerCase() ?? "")).length;
    if (degraded > 0) {
      addSignal({
        evidenceId: evidence.id,
        sourceTool: evidence.sourceTool,
        code: "service_degraded",
        severity: "critical",
        confidence: "medium",
        summary: `服务摘要中有 ${degraded} 个服务处于 stopped/failed 状态。`,
        count: degraded,
      });
    }
  }

  if (evidence.sourceTool === "docker.ps") {
    const containers = Array.isArray(content.containers) ? content.containers : [];
    const notRunning = containers.filter((container) => stringValue(asRecord(container).state)?.toLowerCase() !== "running").length;
    const restarting = containers.filter((container) => {
      const restartCount = asRecord(container).restartCount;
      return typeof restartCount === "number" && restartCount > 0;
    }).length;
    if (notRunning > 0) {
      addSignal({
        evidenceId: evidence.id,
        sourceTool: evidence.sourceTool,
        code: "container_not_running",
        severity: "critical",
        confidence: "medium",
        summary: `Docker 摘要中有 ${notRunning} 个容器不是 running 状态。`,
        count: notRunning,
      });
    }
    if (restarting > 0) {
      addSignal({
        evidenceId: evidence.id,
        sourceTool: evidence.sourceTool,
        code: "container_restart_history",
        severity: "warning",
        confidence: "low",
        summary: `Docker 摘要中有 ${restarting} 个容器存在重启计数。`,
        count: restarting,
      });
    }
  }

  if (evidence.sourceTool === "docker.inspect") {
    const state = stringValue(content.state)?.toLowerCase();
    const health = stringValue(content.health)?.toLowerCase();
    if (state && state !== "running") {
      addSignal({
        evidenceId: evidence.id,
        sourceTool: evidence.sourceTool,
        code: "container_not_running",
        severity: "critical",
        confidence: "medium",
        summary: `目标容器当前为 ${boundText(state, 80)} 状态。`,
      });
    }
    if (health && !["healthy", "none", "unknown"].includes(health)) {
      addSignal({
        evidenceId: evidence.id,
        sourceTool: evidence.sourceTool,
        code: "container_health_degraded",
        severity: "warning",
        confidence: "medium",
        summary: `目标容器 health=${boundText(health, 80)}。`,
      });
    }
  }

  if (evidence.sourceTool === "systemd.inspect") {
    const active = stringValue(content.activeState)?.toLowerCase();
    if (active && active !== "active") {
      addSignal({
        evidenceId: evidence.id,
        sourceTool: evidence.sourceTool,
        code: "service_degraded",
        severity: "critical",
        confidence: "medium",
        summary: `systemd 服务 activeState=${boundText(active, 80)}。`,
      });
    }
  }
}

function buildHypotheses(signals: TriageSignal[], focus?: string): TriageOutput["hypotheses"] {
  const byCode = new Map<string, TriageSignal[]>();
  for (const item of signals) {
    const existing = byCode.get(item.code) ?? [];
    existing.push(item);
    byCode.set(item.code, existing);
  }
  const sourceIds = (codes: string[]): string[] => [...new Set(signals.filter((item) => codes.includes(item.code)).map((item) => item.evidenceId))].slice(0, MAX_EVIDENCE);
  const signalCodes = (codes: string[]): string[] => [...new Set(codes.filter((code) => byCode.has(code)))].slice(0, 16);
  const result: TriageOutput["hypotheses"] = [];
  const add = (candidate: TriageOutput["hypotheses"][number]): void => {
    if (candidate.evidenceIds.length > 0 && result.length < MAX_HYPOTHESES) result.push(candidate);
  };
  const addCompound = (candidate: TriageOutput["hypotheses"][number]): void => {
    // A compound candidate must be supported by at least two persisted items. A
    // single envelope can contain several fields, but it cannot establish that
    // independent sources agree with one another.
    if (new Set(candidate.evidenceIds).size >= 2 && result.length < MAX_HYPOTHESES) result.push(candidate);
  };

  add({
    code: "resource_pressure_candidate",
    title: "资源压力候选",
    statement: `${focus ? `围绕“${boundText(focus, 100)}”，` : ""}已有证据出现高资源使用信号；这只能作为优先核对方向，不能证明资源压力是根因。`,
    evidenceIds: sourceIds(["resource_pressure"]),
    signalCodes: signalCodes(["resource_pressure"]),
    confidence: "medium",
    nextVerification: "重新采集当前 CPU、内存和磁盘指标，并与同一目标范围的近期日志对照。",
  });
  add({
    code: "service_availability_candidate",
    title: "服务可用性候选",
    statement: "已有证据出现停止、失败或非 active 的服务/容器信号；这只说明可用性值得优先核对，不等于已经确定故障传播路径。",
    evidenceIds: sourceIds(["service_degraded", "container_not_running", "container_health_degraded"]),
    signalCodes: signalCodes(["service_degraded", "container_not_running", "container_health_degraded"]),
    confidence: "medium",
    nextVerification: "在相同范围重新检查目标服务/容器状态，并查看相邻时间段的日志错误模式。",
  });
  add({
    code: "error_burst_candidate",
    title: "错误日志候选",
    statement: "日志正文中出现受限错误模式或 error/warning 级别记录；这是文本线索，不是因果判断，需用时间窗和组件状态继续核对。",
    evidenceIds: sourceIds(["log_error_level", "log_out_of_memory", "log_timeout", "log_connection_refused", "log_panic_or_exception"]),
    signalCodes: signalCodes(["log_error_level", "log_out_of_memory", "log_timeout", "log_connection_refused", "log_panic_or_exception"]),
    confidence: "low",
    nextVerification: "取回相关日志证据的必要片段，并以同一运行或有界时间窗比较前后样本，不要直接执行变更。",
  });
  add({
    code: "incomplete_observation_candidate",
    title: "观测不完整候选",
    statement: "当前资料含截断、过期或采集器失败警告，任何结论都应先补采样而不是当作当前事实。",
    evidenceIds: sourceIds(["evidence_truncated", "collector_unavailable"]),
    signalCodes: signalCodes(["evidence_truncated", "collector_unavailable"]),
    confidence: "medium",
    nextVerification: "重新采集缺失或过期的只读证据，并把截断/采集失败保留在 Finding 中。",
  });
  addCompound({
    code: "resource_error_alignment_candidate",
    title: "资源与日志信号共同出现",
    statement: "不同证据同时出现资源高水位与 OOM/超时文本信号；这只说明两类信号值得一起核对，不证明资源压力导致了日志错误。",
    evidenceIds: sourceIds(["resource_pressure", "log_out_of_memory", "log_timeout"]),
    signalCodes: signalCodes(["resource_pressure", "log_out_of_memory", "log_timeout"]),
    confidence: "medium",
    nextVerification: "在同一目标范围重新采集资源指标和相邻时间段日志，比较采集时间与组件状态后再判断是否存在时间上的一致性。",
  });
  addCompound({
    code: "service_error_alignment_candidate",
    title: "服务状态与错误日志共同出现",
    statement: "不同证据同时出现服务/容器不可用与错误日志信号；这只是优先核对的交叉线索，不证明某条错误日志就是服务停止的原因。",
    evidenceIds: sourceIds([
      "service_degraded",
      "container_not_running",
      "container_health_degraded",
      "log_error_level",
      "log_connection_refused",
      "log_panic_or_exception",
    ]),
    signalCodes: signalCodes([
      "service_degraded",
      "container_not_running",
      "container_health_degraded",
      "log_error_level",
      "log_connection_refused",
      "log_panic_or_exception",
    ]),
    confidence: "medium",
    nextVerification: "重新检查同一服务/容器的状态，并取回相邻时间窗的必要日志片段，确认时间顺序而不是只看共现。",
  });
  addCompound({
    code: "container_restart_instability_candidate",
    title: "容器重启与不可用共同出现",
    statement: "容器清单同时显示重启计数和非 running/不健康状态；这提示需要核对启动失败或反复重启，但不能推出具体配置或镜像是根因。",
    evidenceIds: sourceIds(["container_restart_history", "container_not_running", "container_health_degraded"]),
    signalCodes: signalCodes(["container_restart_history", "container_not_running", "container_health_degraded"]),
    confidence: "medium",
    nextVerification: "重新检查目标容器、退出状态和有界日志，确认重启计数的时间范围及当前是否仍在重启。",
  });
  return result;
}

function asRecord(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function stringValue(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

function boundText(value: string, max: number): string {
  return [...value].slice(0, max).join("") + (value.length > max ? "…" : "");
}

export const investigationEvidenceTriageInputSchema = input;
export const investigationEvidenceTriageOutputSchema = output;

import type {
  DecisionOptionContinuation,
  Evidence,
  InvestigationPlan,
  InvestigationStep,
  TaskStatus,
} from "@yukinal/shared";

export const STATUS_LABEL: Record<TaskStatus, string> = {
  pending: "待开始",
  investigating: "排查中",
  waiting_user: "等待选择",
  executing: "执行中",
  verifying: "验证中",
  completed: "已完成",
  failed: "失败",
  stopped: "已停止",
  expired: "已过期",
};

export const OBSERVATION_STATUS_LABEL: Record<NonNullable<InvestigationPlan["observationWindow"]>["status"], string> = {
  pending: "待开始",
  running: "进行中",
  succeeded: "已完成",
  failed: "发现异常",
  cancelled: "已取消",
};

export const STEP_STATUS_LABEL: Record<InvestigationStep["status"], string> = {
  pending: "待开始",
  running: "执行中",
  waiting_user: "等待批准",
  succeeded: "成功",
  failed: "失败",
  skipped: "跳过",
};

export const STEP_KIND_LABEL: Record<InvestigationStep["kind"], string> = {
  plan: "计划",
  evidence: "取证",
  decision: "判断",
  action: "行动",
  verification: "验证",
  recovery: "恢复",
};

export function freshnessLabel(status: NonNullable<Evidence["freshness"]>["status"]): string {
  return { fresh: "新鲜", stale: "已变旧", expired: "已过期", unknown: "未知" }[status];
}

export function continuationLabel(continuation?: DecisionOptionContinuation): string {
  return {
    continue_readonly: "继续有限只读复核",
    start_plan: "启动已选择的计划",
    wait_user: "等待下一步输入",
    stop: "保持停止，不启动新运行",
  }[continuation ?? "wait_user"];
}

export function previewEvidence(content: unknown): string {
  let text: string;
  if (typeof content === "string") {
    text = content;
  } else {
    try {
      text = JSON.stringify(content);
    } catch {
      text = String(content);
    }
  }
  return text.length > 1_200 ? `${text.slice(0, 1_200)}…` : text;
}

export function formatBytes(bytes: number): string {
  if (bytes < 1_024) return `${bytes} B`;
  if (bytes < 1_024 * 1_024) return `${(bytes / 1_024).toFixed(1)} KB`;
  return `${(bytes / (1_024 * 1_024)).toFixed(1)} MB`;
}

export function splitLines(value: string): string[] {
  return value.split("\n").map((line) => line.trim()).filter(Boolean);
}

export function localInputToIso(value: string): string | undefined {
  if (!value.trim()) return undefined;
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? value : parsed.toISOString();
}

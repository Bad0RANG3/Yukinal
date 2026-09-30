import {
  errorCategoryFromTaskFailure,
  type ErrorCategory,
  type InvestigationFailure,
} from "@yukinal/shared";

/**
 * Category-driven presentation for a task failure.
 *
 * The screen must answer "where did this happen, what is the state now, what can
 * I do next" from a stable category, never by matching the message text. The
 * category comes from the shared taxonomy so the Rust host and this UI agree on
 * what each wire code means.
 */
export interface FailureDisplay {
  label: string;
  nextStep: string;
}

const CATEGORY_DISPLAY: Record<ErrorCategory, FailureDisplay> = {
  input: { label: "输入不合法", nextStep: "修正输入或附件后重试；宿主已拒绝这次执行。" },
  permission: { label: "权限不足", nextStep: "检查目标环境与权限档位，必要时缩小范围后重新发起。" },
  approval: { label: "等待批准", nextStep: "回到审批卡逐项确认，或调整计划后重试。" },
  authentication: { label: "认证失败", nextStep: "重新绑定该服务器的凭据，再重试。" },
  transport: { label: "连接中断", nextStep: "检查网络与 sidecar 状态，可直接重试。" },
  timeout: { label: "等待超时", nextStep: "确认目标是否仍可用，缩小范围后可重试。" },
  outcome_unknown: { label: "远端结果未知", nextStep: "命令或变更可能已经生效。先查看实际执行记录并重新采集只读证据，不要直接重试。" },
  cancelled: { label: "已被取消", nextStep: "查看现场证据，确认后决定是否恢复。" },
  budget: { label: "预算耗尽", nextStep: "调整任务范围或预算上限后重新规划。" },
  not_found: { label: "目标不存在", nextStep: "重新确认服务器、路径或服务名后重试。" },
  unsupported: { label: "目标不支持该操作", nextStep: "换用受支持的操作路径，重复重试不会改变结果。" },
  remote_failure: { label: "远端执行失败", nextStep: "查看远端输出与证据，修正命令或目标后重试。" },
  output: { label: "输出被截断", nextStep: "缩小查询范围或提高上限后重新采集，当前结果不完整。" },
  evidence: { label: "证据不足", nextStep: "补充同一范围内的只读采样，或重新规划。" },
  stale: { label: "目标状态已变化", nextStep: "重新读取目标状态、刷新基线和批准，再重试。" },
  plan: { label: "偏离当前计划", nextStep: "要求 Agent 基于现有证据生成新的计划修订。" },
  internal: { label: "内部错误", nextStep: "记录发生位置后重试；若重复出现，检查 sidecar 日志。" },
  unknown: { label: "未分类失败", nextStep: "查看证据与日志后决定下一步；宿主没有猜成成功。" },
};

export function describeInvestigationFailure(
  failure: Pick<InvestigationFailure, "code">,
): FailureDisplay {
  return CATEGORY_DISPLAY[errorCategoryFromTaskFailure(failure.code)];
}

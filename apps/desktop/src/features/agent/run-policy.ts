/**
 * 「这次运行按哪套策略判定」的展示文案与选项表。
 *
 * 和 `run-mode.ts` 同一条理由独立成模块：这里的措辞直接决定用户对「谁在约束这次运行」
 * 的理解。策略是**安全相关**的，文案写错一个字比崩溃更危险，所以它必须是可被测试钉住
 * 的纯数据，而不是散在 JSX 里的字符串。
 *
 * 三条必须说清的事实，也就是这一屏文案的全部责任：
 *
 *  1. 策略覆盖「按环境自动」。选了某一套，它就**完全取代**目标环境推导出的那套 ——
 *     即使目标环境与它不一致。这是这个选项存在的意义，也不能被含糊过去。
 *  2. 只读与高危的边界不由策略决定。四套内建策略的 `read` 都是 auto，而
 *     high / critical 无论选哪一套都必须由用户逐项批准（权限引擎里那条「dangerous
 *     不能自动批准」的规则），所以选任何一套都换不来「危险操作自动执行」。
 *  3. 「按环境自动」用的是内建表，不是「没有策略」。环境未知的目标按生产策略处理。
 *
 * 键取自 `@yukinal/shared` 的内建策略 id（`BuiltinPolicyId`），所以少写一套策略会直接
 * 编译失败，而不是在界面上悄悄缺一行。
 */

import { BUILTIN_POLICY_IDS, defaultPolicyFor, type BuiltinPolicyId, type Environment } from "@yukinal/shared";

/**
 * 用户的选择。`null` 是「按环境自动」：请求里**不带** `policyId`，由 sidecar 用目标
 * 环境推导。用 `null` 而不是某个字符串字面量，是为了让「没选」与「选了一套策略」在
 * 类型上就分开，拼不出一个会被当成策略 id 的默认值。
 */
export type RunPolicyChoice = BuiltinPolicyId | null;

export interface RunPolicySpec {
  value: RunPolicyChoice;
  label: string;
  /** 菜单里的一行说明：这套策略会对什么放行、对什么停下来。 */
  summary: string;
}

/** 默认项。它不是一个策略，而是「让目标环境决定」。 */
export const AUTO_RUN_POLICY: RunPolicySpec = {
  value: null,
  label: "按环境自动",
  summary: "由目标环境决定用哪套内建策略（本机 / 开发 / 预发布 / 生产）；环境未知的目标按生产策略处理。",
};

export const RUN_POLICY_SPECS: Readonly<Record<BuiltinPolicyId, RunPolicySpec>> = {
  "policy.local": {
    value: "policy.local",
    label: "本机策略",
    summary: "本机目标：读取自动放行，写入与高危操作都会先暂停等你批准。",
  },
  "policy.development": {
    value: "policy.development",
    label: "开发策略",
    summary: "开发目标：普通写入自动放行，高危操作仍会先暂停等你批准。",
  },
  "policy.staging": {
    value: "policy.staging",
    label: "预发布策略",
    summary: "预发布目标：普通写入自动放行，高危操作仍会先暂停等你批准。",
  },
  "policy.production": {
    value: "policy.production",
    label: "生产策略",
    summary: "生产目标：写入与高危操作都必须逐项批准。",
  },
};

/** 菜单渲染顺序：先「按环境自动」，再按环境由宽到严。 */
export const RUN_POLICY_ORDER: readonly RunPolicyChoice[] = [null, ...BUILTIN_POLICY_IDS];

export function runPolicySpec(policy: RunPolicyChoice): RunPolicySpec {
  return policy === null ? AUTO_RUN_POLICY : RUN_POLICY_SPECS[policy];
}

/**
 * 请求里要带的 `policyId`：**永远没有这个字段** —— 无论用户选了哪一项。
 *
 * 这个 Python Agent Node 里没有策略引擎：`system.describe` 返回的 `permissionPolicyIds`
 * 是空数组，`ToolRegistry` 从不接 `authorization`，所以没有任何一套策略会被应用到这次
 * 运行上。而请求里只要出现 `policyId`，sidecar 就一律以 `-32602` 拒掉整次
 * `agent_run_start`（`policy selection is not supported by this Python Agent Node yet`）——
 * 把它发出去并不会「多一条约束」，而是让 run 直接启动失败。
 *
 * 所以这里恒返回 `undefined`。这不是「忽略用户的选择」，而是承认界面上的选择目前
 * **只是界面状态**：`AgentComposer` 里那排按钮因此被禁用并附上说明，而不是留一个
 * 「能点、但一点就让运行失败」的控件。将来若这个节点真的接上了策略注册表，
 * 要改的是这一处换算与它上面的说明，而不是把选择直接透传出去。
 *
 * 单独成一个函数是因为这是界面上唯一一处「用户的选择 → 请求字段」的换算点，
 * 这条规则必须能被断言，而不是只存在于 JSX 里。
 */
export function requestPolicyId(policy: RunPolicyChoice): string | undefined {
  // 参数保留在签名里：换算点拿到的仍然是界面上的那个选择，而这个函数对它一律无视 ——
  // 这正是要被钉住的规则本身。
  void policy;
  return undefined;
}

const ENVIRONMENT_LABELS: Record<Environment, string> = {
  local: "本机",
  development: "开发",
  staging: "预发布",
  production: "生产",
  unknown: "未知（按生产处理）",
};

/** Warn when an explicit policy conflicts with the environment's default policy. */
export function policyEnvironmentWarning(
  policy: RunPolicyChoice,
  environment: Environment,
): string | null {
  if (policy === null || defaultPolicyFor(environment).id === policy) return null;
  return `目标环境是${ENVIRONMENT_LABELS[environment]}，但选择了${runPolicySpec(policy).label}；本次运行会按所选策略判定。`;
}

/**
 * 运行模式的展示文案与「谁批准」的两档选择。
 *
 * 这些是纯数据，抽出来单测 —— 模式的措辞会直接决定用户对「这次运行能做什么」
 * 的理解，写错一个字比崩溃更危险，所以要能被测试钉住。
 *
 * 两条轴互相正交，必须分开呈现：
 *   - 运行模式（mode）：这次运行**允许做到什么程度**
 *   - 批准方式（permissionMode）：允许的部分**由谁点头**
 * 「计划模式 + 自动批准」是合法组合，两者不是二选一。
 */

import type { AgentPermissionMode, AgentRunMode } from "@yukinal/shared";

export interface RunModeSpec {
  mode: AgentRunMode;
  label: string;
  /** 菜单里的一行说明：说清楚这次运行能做什么、不能做什么。 */
  summary: string;
  /** 只读类模式的菜单项会带这个标记，让「点什么都不会改」一眼可见。 */
  readOnly: boolean;
}

/**
 * 用 Record 而不是数组查找：每个模式都必须有文案这件事由类型强制，
 * 新增模式却忘了写说明会直接编译失败，而不是回退到某个默认文案。
 */
export const RUN_MODE_SPECS: Readonly<Record<AgentRunMode, RunModeSpec>> = {
  goal: {
    mode: "goal",
    label: "目标模式",
    summary: "以完成目标为准，可连续多步执行；改动照常按批准方式处理。",
    readOnly: false,
  },
  plan: {
    mode: "plan",
    label: "计划模式",
    summary: "只调查并给出可执行计划，不执行任何变更。",
    readOnly: true,
  },
  readonly: {
    mode: "readonly",
    label: "只读模式",
    summary: "只看不改：写入、部署、重启会被直接拒绝。",
    readOnly: true,
  },
};

/** 菜单渲染顺序：由宽到严，让「限制更紧」感觉是往下走。 */
export const RUN_MODE_ORDER: readonly AgentRunMode[] = ["goal", "plan", "readonly"];

export function runModeSpec(mode: AgentRunMode): RunModeSpec {
  return RUN_MODE_SPECS[mode];
}

export interface ApprovalOptionSpec {
  value: AgentPermissionMode;
  label: string;
  summary: string;
}

/**
 * 「谁批准」不是开关而是两档：原来那个「帮我批准」勾选框把「请求批准」当成
 * 未勾选状态，用户看到的是一个空框，读不出「不勾 = 每次都会问你」。
 */
export const APPROVAL_OPTIONS: Readonly<Record<AgentPermissionMode, ApprovalOptionSpec>> = {
  ask: {
    value: "ask",
    label: "请求批准",
    summary: "写入、部署、重启等操作都会先暂停，等你点头。",
  },
  auto: {
    value: "auto",
    label: "帮我批准",
    summary: "仅在开发/预发布目标上自动放行普通写入；高危操作仍会问你。",
  },
};

/** 菜单渲染顺序：「更谨慎」在前。 */
export const APPROVAL_ORDER: readonly AgentPermissionMode[] = ["ask", "auto"];

export function approvalOptionSpec(mode: AgentPermissionMode): ApprovalOptionSpec {
  return APPROVAL_OPTIONS[mode];
}

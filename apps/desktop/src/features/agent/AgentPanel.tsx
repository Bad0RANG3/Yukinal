/**
 * Agent 面板：工作台而非聊天窗。
 *
 * 渲染循环将要填充的形状：状态、工具卡片、审批位。没有假 transcript。
 */

import type { AgentRunState } from "@yukinal/shared";

const STATES: AgentRunState[] = [
  "idle",
  "thinking",
  "running_tool",
  "waiting_approval",
  "completed",
  "failed",
  "cancelled",
];

export function AgentPanel() {
  return (
    <aside className="flex w-96 shrink-0 flex-col bg-zinc-950/60">
      <header className="flex items-center justify-between border-b border-zinc-800 px-3 py-2">
        <span className="text-sm font-medium">Agent</span>
        <span className="text-xs text-zinc-500">provider：未配置</span>
      </header>

      <div className="flex-1 space-y-2 overflow-auto p-3">
        <p className="text-sm text-zinc-400">
          用目标提问，而不是命令：“为什么 staging API 在重启？”
        </p>

        <ol className="space-y-1 text-xs text-zinc-500">
          {STATES.map((state) => (
            <li key={state} className="flex items-center gap-2">
              <span className="h-1.5 w-1.5 rounded-full bg-zinc-700" />
              {state}
            </li>
          ))}
        </ol>

        <ToolCardPlaceholder />
        <ApprovalPlaceholder />
      </div>

      <footer className="border-t border-zinc-800 p-3">
        <textarea
          rows={3}
          placeholder="连接服务器并配置 provider 后即可开始"
          className="w-full resize-none rounded-md border border-zinc-800 bg-zinc-900 p-2 text-sm outline-none placeholder:text-zinc-600"
          disabled
        />
      </footer>
    </aside>
  );
}

/** 每次工具调用一张卡片，可展开输入/输出。 */
function ToolCardPlaceholder() {
  return (
    <div className="rounded-md border border-zinc-800 p-2 text-xs text-zinc-500">
      <div className="font-medium text-zinc-400">工具卡片（docker__logs）</div>
      <p>服务器 · 容器 · 状态 · 查看输出 —— 尚未接线。</p>
    </div>
  );
}

/** 危险操作浮出解析后的目标与风险。 */
function ApprovalPlaceholder() {
  return (
    <div className="rounded-md border border-amber-500/40 bg-amber-500/5 p-2 text-xs text-amber-200">
      <div className="font-medium">生产环境变更需要审批</div>
      <p className="text-amber-200/70">[取消] [批准一次] [本次会话批准] —— 尚未接线。</p>
    </div>
  );
}
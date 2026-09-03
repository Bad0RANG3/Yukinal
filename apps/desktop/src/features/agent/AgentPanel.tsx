import type { AgentRunState } from "@yukinal/shared";

/**
 * The Agent panel is a workspace, not a chat window (.2).
 *
 * This renders the *shape* the loop will fill: state, the tool cards that make
 * each step observable, and the approval affordance that dangerous
 * actions require. No transcript is faked in the meantime.
 */
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
        <span className="text-xs text-zinc-500">provider: not configured</span>
      </header>

      <div className="flex-1 space-y-2 overflow-auto p-3">
        <p className="text-sm text-zinc-400">
          Ask about an environment, not a command: “why is the staging API restarting?”
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
          placeholder="Connect a server and configure a provider to start"
          className="w-full resize-none rounded-md border border-zinc-800 bg-zinc-900 p-2 text-sm outline-none placeholder:text-zinc-600"
          disabled
        />
      </footer>
    </aside>
  );
}

/** one card per tool call, expandable to input + output. */
function ToolCardPlaceholder() {
  return (
    <div className="rounded-md border border-zinc-800 p-2 text-xs text-zinc-500">
      <div className="font-medium text-zinc-400">Tool card (docker__logs)</div>
      <p>Server · Container · Status · View output — not wired yet.</p>
    </div>
  );
}

/** dangerous actions surface the resolved target and the risk. */
function ApprovalPlaceholder() {
  return (
    <div className="rounded-md border border-amber-500/40 bg-amber-500/5 p-2 text-xs text-amber-200">
      <div className="font-medium">Production change requires approval</div>
      <p className="text-amber-200/70">[Cancel] [Approve once] [Approve for this session] — not wired yet.</p>
    </div>
  );
}

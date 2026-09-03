import { useQuery } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";
import { EnvBadge } from "../../components/EnvBadge.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";

/**
 * Default page (Principle 1). Mirrors the layout: status, host facts, three
 * gauges, containers, attention, recent activity.
 *
 * It renders structure without inventing data: the numbers come from
 * `yukinal-collector`, and the attention line from the health translation.
 */
export function ServerOverview() {
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);

  const snapshot = useQuery({
    queryKey: ["snapshot", selectedServerId],
    enabled: isDesktopShell() && selectedServerId !== null,
    queryFn: async () =>
      (await callDesktop(IPC_COMMANDS.serverSnapshot, { serverId: selectedServerId ?? "" })).snapshot,
  });

  return (
    <section className="space-y-4">
      <header className="flex items-center gap-3">
        <h1 className="text-lg font-semibold">
          {selectedServerId ? `Server ${selectedServerId}` : "No server selected"}
        </h1>
        <EnvBadge environment="unknown" serverName="unresolved" />
      </header>

      <div className="grid grid-cols-3 gap-3">
        <Gauge label="CPU" value={snapshot.data?.cpu?.usagePercent} />
        <Gauge label="Memory" value={snapshot.data?.memory?.usagePercent} />
        <Gauge label="Disk" value={snapshot.data?.disks?.[0]?.usagePercent} />
      </div>

      <div>
        <h2 className="mb-1 text-xs uppercase tracking-wide text-zinc-500">Docker</h2>
        {snapshot.data?.docker?.containers?.length ? (
          <ul className="space-y-1">
            {snapshot.data.docker.containers.map((container) => (
              <li key={container.name} className="flex items-center gap-2 text-zinc-300">
                <Dot state={container.state} />
                {container.name}
                <span className="text-xs text-zinc-500">{container.status}</span>
              </li>
            ))}
          </ul>
        ) : (
          <p className="text-sm text-zinc-500">Waiting for the collector.</p>
        )}
      </div>
    </section>
  );
}

function Gauge({ label, value }: { label: string; value?: number }) {
  const percent = value ?? 0;
  return (
    <div className="rounded-lg border border-zinc-800 p-3">
      <div className="flex items-baseline justify-between">
        <span className="text-xs uppercase tracking-wide text-zinc-500">{label}</span>
        <span className="text-sm text-zinc-300">{value === undefined ? "—" : `${Math.round(value)}%`}</span>
      </div>
      <div className="mt-2 h-1.5 overflow-hidden rounded bg-zinc-800">
        <div className="h-full bg-sky-500/70" style={{ width: `${Math.min(100, percent)}%` }} />
      </div>
    </div>
  );
}

function Dot({ state }: { state: string }) {
  const color = state === "running" ? "bg-emerald-400" : state === "restarting" ? "bg-amber-400" : "bg-red-400";
  return <span className={`h-2 w-2 rounded-full ${color}`} />;
}

import { useState } from "react";
import { WorkpageContent } from "../features/workpages/WorkpagesDock.js";
import { WORKPAGE_LABEL, WORKPAGE_TOOLS, type WorkpageTool } from "../features/workpages/workpage-model.js";
import { hideNativeWorkpage, pinNativeWorkpage } from "../lib/ipc.js";
import { useServers } from "../lib/servers.js";

export function readWorkpageWindow(): { kind: WorkpageTool; serverId: string } | null {
  const params = new URLSearchParams(window.location.search);
  const kind = params.get("workpage");
  const serverId = params.get("server");
  return WORKPAGE_TOOLS.some((tool) => tool === kind) && serverId && /^[A-Za-z0-9_-]{1,256}$/u.test(serverId)
    ? { kind: kind as WorkpageTool, serverId } : null;
}

export function WorkpageWindow({ kind, serverId }: { kind: WorkpageTool; serverId: string }) {
  const [pinned, setPinned] = useState(false);
  const [changing, setChanging] = useState(false);
  const [error, setError] = useState("");
  const servers = useServers({ poll: true });
  const server = servers.data?.find((item) => item.id === serverId);
  return <main className="standalone-workpage">
    <header><strong>{WORKPAGE_LABEL[kind]}</strong><span>{server?.name ?? serverId}</span>
      <button type="button" className="text-button" aria-pressed={pinned} disabled={changing} onClick={async () => {
        setChanging(true); setError("");
        try { setPinned(await pinNativeWorkpage(!pinned)); } catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)); }
        finally { setChanging(false); }
      }}>{pinned ? "取消置顶" : "置顶"}</button>
      <button type="button" className="text-button" onClick={() => void hideNativeWorkpage()}>收起</button>
    </header>
    {error ? <p role="alert" className="inline-error">{error}</p> : null}
    <div className="standalone-workpage-content"><WorkpageContent page={{ id: `standalone-${kind}-${serverId}`, title: WORKPAGE_LABEL[kind], serverId, kind, dock: "right" }} active /></div>
  </main>;
}

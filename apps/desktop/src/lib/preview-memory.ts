import { IPC_COMMANDS, type AgentMemory, type IpcCommandName } from "@yukinal/shared";

const now = new Date().toISOString();
const memories: AgentMemory[] = [
  { id: "mem_preview_global", scope: "global", title: "排查约定", content: "先读取证据，再提出变更；维护窗口内执行需要批准的操作。", createdAt: now, updatedAt: now },
  { id: "mem_preview_database", scope: "server", serverId: "srv_previewdb", title: "数据库备份", content: "备份目录 /srv/backups，预发布数据库连接池上限 48。", createdAt: now, updatedAt: now },
  { id: "mem_preview_api", scope: "server", serverId: "srv_previewapi", title: "API 部署路径", content: "应用位于 /srv/app，服务由 systemd 管理。", createdAt: now, updatedAt: now },
];

export function previewMemoryResponse(command: IpcCommandName, raw: unknown): unknown {
  const params = raw as Record<string, unknown>;
  if (command === IPC_COMMANDS.agentMemoryList) return { memories: memories.filter((memory) =>
    (!params.scope || memory.scope === params.scope) && (memory.scope === "global" || memory.serverId === params.serverId) &&
    (!params.query || `${memory.title} ${memory.content}`.toLowerCase().includes(String(params.query).toLowerCase()))) };
  if (command === IPC_COMMANDS.agentMemorySave) {
    const index = memories.findIndex((memory) => memory.id === params.id);
    const existing = memories[index];
    if (existing && (existing.scope !== params.scope || existing.serverId !== params.serverId)) throw new Error("记忆范围不匹配");
    const memory: AgentMemory = { id: existing?.id ?? `mem_preview_${crypto.randomUUID()}`, scope: params.scope as AgentMemory["scope"],
      ...(typeof params.serverId === "string" ? { serverId: params.serverId } : {}), title: String(params.title), content: String(params.content), createdAt: existing?.createdAt ?? now, updatedAt: new Date().toISOString() };
    if (index < 0) memories.push(memory); else memories[index] = memory;
    return { memory };
  }
  if (command === IPC_COMMANDS.agentMemoryDelete) {
    const index = memories.findIndex((memory) => memory.id === params.id && memory.scope === params.scope && memory.serverId === params.serverId);
    if (index >= 0) memories.splice(index, 1);
    return { deleted: index >= 0 };
  }
  if (command === IPC_COMMANDS.agentContextReset) return { reset: true };
  if (command === IPC_COMMANDS.agentContextResetAll) return { reset: true, count: 0 };
  return undefined;
}

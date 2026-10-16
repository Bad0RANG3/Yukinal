import type { Server, WindowShellAction } from "@yukinal/shared";

export function dispatchWindowShellAction(action: WindowShellAction, services: {
  servers: readonly Server[];
  blocked: boolean;
  navigate: (section: "servers" | "projects" | "tasks" | "activity" | "settings") => void;
  select: (id: string) => void;
  add: () => void;
  refresh: () => void;
  edit: (server: Server) => void;
  confirmDelete: (server: Server) => boolean;
  mutate: (input: { type: "connect" | "disconnect" | "delete"; serverId: string }) => void;
}): void {
  if (services.blocked) return;
  if (action.type === "navigate") { services.navigate(action.section); return; }
  if (action.type === "add-server") { services.add(); return; }
  if (action.type === "refresh-servers") { services.refresh(); return; }
  if (!("serverId" in action)) return;
  const server = services.servers.find((item) => item.id === action.serverId);
  if (!server) return;
  if (action.type === "select-server") { services.select(server.id); return; }
  if (action.type === "edit-server") { services.edit(server); return; }
  if (action.type === "delete-server") {
    if (services.confirmDelete(server)) services.mutate({ type: "delete", serverId: server.id });
  } else if (action.type === "connect-server" && server.status !== "connected" && server.status !== "connecting") {
    services.mutate({ type: "connect", serverId: server.id });
  } else if (action.type === "disconnect-server" && server.status === "connected") {
    services.mutate({ type: "disconnect", serverId: server.id });
  }
}

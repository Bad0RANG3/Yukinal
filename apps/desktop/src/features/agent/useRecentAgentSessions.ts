import { IPC_COMMANDS } from "@yukinal/shared";
import { useQuery } from "@tanstack/react-query";

import { callDesktop, isDesktopShell } from "../../lib/ipc.js";

/** A server ID is always explicit: null is the global workspace. */
export function useRecentAgentSessions(serverId: string | null, limit = 8) {
  return useQuery({
    queryKey: ["agent-chat-sessions", "recent", serverId, limit],
    enabled: isDesktopShell(),
    queryFn: () => callDesktop(IPC_COMMANDS.chatSessionList, { serverId, archived: false, limit }),
    staleTime: 15_000,
  });
}

import type { IpcCommandName } from "@yukinal/shared";

const CHAT_COMMANDS = new Set<IpcCommandName>([
  "agent_status", "agent_spawn", "agent_logs", "agent_run_start", "agent_run_stop",
  "agent_context_reset", "agent_context_reset_all",
  "provider_list", "provider_save", "provider_activate", "provider_delete", "provider_models", "provider_test",
  "chat_session_list", "chat_session_get", "chat_session_create", "chat_message_append",
  "chat_session_archive", "chat_session_rename", "chat_session_delete",
]);

export function usesNativePreviewChat(command: IpcCommandName, native: boolean): boolean {
  return native && CHAT_COMMANDS.has(command);
}

/** Mock IDs never cross into native persistence or host tools. */
export function nativePreviewChatParams(command: IpcCommandName, params: Record<string, unknown>): Record<string, unknown> {
  const result = { ...params };
  if (["chat_session_list", "chat_session_create", "agent_context_reset"].includes(command)) {
    delete result.serverId;
    delete result.focusServerId;
  }
  if (command === "agent_run_start") {
    for (const field of ["taskId", "workspaceId", "focusServerId", "target", "policyId", "planBinding"]) delete result[field];
    result.toolAccess = "none";
    result.mode = "readonly";
    result.permissionMode = "ask";
  }
  return result;
}

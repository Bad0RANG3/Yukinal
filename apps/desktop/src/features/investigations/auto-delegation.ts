import type {
  AgentPermissionMode,
  AgentRunMode,
  TaskAutomationLevel,
  ToolTarget,
} from "@yukinal/shared";

/**
 * The task UI may expose the Agent's run-level `auto` delegation only where the
 * Permission Engine can actually honour it: an executable goal on a remote
 * development or staging target. Keeping this predicate here gives the UI one
 * conservative answer instead of making it infer policy from labels.
 */
export function canDelegateAgentAuto(input: {
  scope: Pick<ToolTarget, "host" | "environment">;
  mode: AgentRunMode;
  automationLevel: TaskAutomationLevel;
}): boolean {
  return (
    input.mode === "goal" &&
    input.automationLevel === "execute" &&
    input.scope.host === "remote" &&
    (input.scope.environment === "development" || input.scope.environment === "staging")
  );
}

/**
 * The primary-form opt-in is one task-level decision: it enables both the
 * bounded host-issued server.exec grant and eligible automatic tool approval.
 * A stale opt-in becomes ask/no-grant as soon as its scope or run mode changes.
 */
export function resolveTaskDelegation(input: {
  requested: boolean;
  scope: Pick<ToolTarget, "host" | "environment"> | undefined;
  mode: AgentRunMode;
  automationLevel: TaskAutomationLevel;
}): {
  eligible: boolean;
  permissionMode: AgentPermissionMode;
  grantTaskCommands: boolean;
} {
  const eligible = Boolean(input.scope && canDelegateAgentAuto({
    scope: input.scope,
    mode: input.mode,
    automationLevel: input.automationLevel,
  }));
  const granted = input.requested && eligible;
  return {
    eligible,
    permissionMode: granted ? "auto" : "ask",
    grantTaskCommands: granted,
  };
}

/**
 * A stale UI selection must never widen the request after the target or mode
 * changes. The host/engine remain the final authority, but normalising here
 * keeps the persisted task honest and the explanation visible to the user.
 */
export function effectiveTaskPermissionMode(
  requested: AgentPermissionMode,
  input: {
    scope: Pick<ToolTarget, "host" | "environment">;
    mode: AgentRunMode;
    automationLevel: TaskAutomationLevel;
  },
): AgentPermissionMode {
  return requested === "auto" && canDelegateAgentAuto(input) ? "auto" : "ask";
}

/**
 * Starting a created task is itself a user-visible side effect. Only the
 * explicitly delegated executable goal may start without a second click; plan
 * tasks and ask-mode goals remain waiting for the user, while readonly tasks
 * keep their existing safe auto-start behaviour.
 */
export function shouldAutoStartCreatedTask(input: {
  mode: AgentRunMode;
  permissionMode: AgentPermissionMode;
  automationLevel: TaskAutomationLevel;
  scope: Pick<ToolTarget, "host" | "environment">;
}): boolean {
  if (input.mode === "readonly") return true;
  return input.permissionMode === "auto" && canDelegateAgentAuto(input);
}

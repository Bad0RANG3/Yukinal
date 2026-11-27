import { useQuery, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";

import { callDesktop, isDesktopShell } from "../../lib/ipc.js";

/**
 * One polling source for the background work the operations workbench shows.
 *
 * The task board, the watch plans and the goal-task page used to fetch the same
 * two lists on three independent schedules (`team-board-tasks` at 2 s,
 * `team-watch` at 2 s, `investigationTasks`/`investigationSchedules` at 2 s/5 s).
 * They now share these two cache entries, so any number of open surfaces costs
 * one request each and every view updates from the same snapshot.
 *
 * Both lists poll at 2 s: a watch plan's own interval is measured in minutes, and
 * the newest run status is what makes a stale board look alive.
 */
export const INVESTIGATION_QUERY_KEYS = {
  tasks: ["investigationTasks"] as const,
  schedules: ["investigationSchedules"] as const,
  task: (taskId: string | null | undefined) => ["investigationTask", taskId] as const,
  scheduleRuns: (scheduleId: string | null | undefined) => ["investigationScheduleRuns", scheduleId] as const,
};

const POLL_MS = 2_000;

export function useInvestigationOverview() {
  const shell = isDesktopShell();
  const tasks = useQuery({
    queryKey: INVESTIGATION_QUERY_KEYS.tasks,
    enabled: shell,
    staleTime: POLL_MS,
    refetchInterval: shell ? POLL_MS : false,
    queryFn: () => callDesktop(IPC_COMMANDS.investigationTaskList, { limit: 100 }),
  });
  const schedules = useQuery({
    queryKey: INVESTIGATION_QUERY_KEYS.schedules,
    enabled: shell,
    staleTime: POLL_MS,
    refetchInterval: shell ? POLL_MS : false,
    queryFn: () => callDesktop(IPC_COMMANDS.investigationScheduleList, {}),
  });
  return { tasks, schedules };
}

/**
 * Refresh the shared snapshot after a mutation. Callers used to spell out the
 * keys themselves; one helper keeps a new key from being forgotten.
 */
export function useInvalidateInvestigations() {
  const queryClient = useQueryClient();
  return () => Promise.all([
    queryClient.invalidateQueries({ queryKey: INVESTIGATION_QUERY_KEYS.tasks }),
    queryClient.invalidateQueries({ queryKey: INVESTIGATION_QUERY_KEYS.schedules }),
  ]);
}

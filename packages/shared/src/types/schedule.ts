import type { TaskBudget, TaskStatus } from "./investigation.js";

/** Durable local trigger state. A trigger never stores provider credentials. */
export const INVESTIGATION_SCHEDULE_STATUSES = ["active", "paused", "revoked"] as const;
export type InvestigationScheduleStatus = (typeof INVESTIGATION_SCHEDULE_STATUSES)[number];

export const INVESTIGATION_SCHEDULE_RUN_STATUSES = [
  "queued",
  "claimed",
  "running",
  "succeeded",
  "failed",
  "skipped",
  "interrupted",
] as const;
export type InvestigationScheduleRunStatus = (typeof INVESTIGATION_SCHEDULE_RUN_STATUSES)[number];

export const INVESTIGATION_NOTIFICATION_POLICIES = ["silent", "on_change", "always", "failed_runs_only"] as const;
export type InvestigationNotificationPolicy = (typeof INVESTIGATION_NOTIFICATION_POLICIES)[number];

export interface InvestigationSchedule {
  id: string;
  taskId: string;
  status: InvestigationScheduleStatus;
  intervalSeconds: number;
  cooldownSeconds: number;
  dedupeWindowSeconds: number;
  maxConcurrentRuns: number;
  budget: TaskBudget;
  notificationPolicy: InvestigationNotificationPolicy;
  nextRunAt: string;
  /** Host-validated successful run used as the explicit comparison baseline. */
  baselineRunId?: string;
  lastRunAt?: string;
  lastOutcome?: string;
  lastError?: string;
  createdAt: string;
  updatedAt: string;
}

export interface InvestigationScheduleRun {
  id: string;
  scheduleId: string;
  taskId: string;
  status: InvestigationScheduleRunStatus;
  scheduledAt: string;
  claimedAt?: string;
  startedAt?: string;
  finishedAt?: string;
  dedupeKey: string;
  outcome?: string;
  error?: string;
}

export interface InvestigationScheduleCreateInput {
  id?: string;
  taskId: string;
  intervalSeconds: number;
  cooldownSeconds?: number;
  dedupeWindowSeconds?: number;
  maxConcurrentRuns?: number;
  budget?: TaskBudget;
  notificationPolicy?: InvestigationNotificationPolicy;
  nextRunAt?: string;
}

export interface InvestigationScheduleUpdateInput {
  scheduleId: string;
  status?: InvestigationScheduleStatus;
  intervalSeconds?: number;
  cooldownSeconds?: number;
  dedupeWindowSeconds?: number;
  maxConcurrentRuns?: number;
  budget?: TaskBudget;
  notificationPolicy?: InvestigationNotificationPolicy;
  nextRunAt?: string;
  /** Select a successful run with evidence, or null to return to the latest-sample baseline. */
  baselineRunId?: string | null;
}

export interface InvestigationScheduleListResponse {
  schedules: InvestigationSchedule[];
}

export interface InvestigationScheduleRunListResponse {
  runs: InvestigationScheduleRun[];
}

export interface InvestigationScheduleTickInput {
  now?: string;
  limit?: number;
}

export interface InvestigationScheduleTickResponse {
  claimed: InvestigationScheduleRun[];
  skipped: InvestigationScheduleRun[];
}

/** Task statuses allowed for a read-only scheduler target. */
export const SCHEDULE_TARGET_STATUSES: TaskStatus[] = ["pending", "investigating", "waiting_user"];

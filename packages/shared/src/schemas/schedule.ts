import { z } from "zod";

import {
  INVESTIGATION_NOTIFICATION_POLICIES,
  INVESTIGATION_SCHEDULE_RUN_STATUSES,
  INVESTIGATION_SCHEDULE_STATUSES,
} from "../types/schedule.js";
import { INVESTIGATION_LIMITS } from "../types/investigation.js";
import { TaskBudgetSchema } from "./investigation.js";

const OPAQUE_ID = z.string().trim().min(1).max(256);
const timestamp = z.string().trim().min(1).max(80);

export const InvestigationScheduleSchema = z.strictObject({
  id: OPAQUE_ID,
  taskId: OPAQUE_ID,
  status: z.enum(INVESTIGATION_SCHEDULE_STATUSES),
  intervalSeconds: z.number().int().positive().max(INVESTIGATION_LIMITS.maxObservationDurationSeconds),
  cooldownSeconds: z.number().int().nonnegative().max(INVESTIGATION_LIMITS.maxObservationDurationSeconds),
  dedupeWindowSeconds: z.number().int().positive().max(INVESTIGATION_LIMITS.maxObservationDurationSeconds),
  maxConcurrentRuns: z.number().int().positive().max(16),
  budget: TaskBudgetSchema,
  notificationPolicy: z.enum(INVESTIGATION_NOTIFICATION_POLICIES),
  nextRunAt: timestamp,
  baselineRunId: OPAQUE_ID.optional(),
  lastRunAt: timestamp.optional(),
  lastOutcome: z.string().max(INVESTIGATION_LIMITS.maxFailureMessageChars).optional(),
  lastError: z.string().max(INVESTIGATION_LIMITS.maxFailureMessageChars).optional(),
  createdAt: timestamp,
  updatedAt: timestamp,
});

export const InvestigationScheduleRunSchema = z.strictObject({
  id: OPAQUE_ID,
  scheduleId: OPAQUE_ID,
  taskId: OPAQUE_ID,
  status: z.enum(INVESTIGATION_SCHEDULE_RUN_STATUSES),
  scheduledAt: timestamp,
  claimedAt: timestamp.optional(),
  startedAt: timestamp.optional(),
  finishedAt: timestamp.optional(),
  dedupeKey: z.string().trim().min(1).max(256),
  outcome: z.string().max(INVESTIGATION_LIMITS.maxFailureMessageChars).optional(),
  error: z.string().max(INVESTIGATION_LIMITS.maxFailureMessageChars).optional(),
});

export const InvestigationScheduleCreateInputSchema = z.strictObject({
  id: OPAQUE_ID.optional(),
  taskId: OPAQUE_ID,
  intervalSeconds: z.number().int().positive().max(INVESTIGATION_LIMITS.maxObservationDurationSeconds),
  cooldownSeconds: z.number().int().nonnegative().max(INVESTIGATION_LIMITS.maxObservationDurationSeconds).optional(),
  dedupeWindowSeconds: z.number().int().positive().max(INVESTIGATION_LIMITS.maxObservationDurationSeconds).optional(),
  maxConcurrentRuns: z.number().int().positive().max(16).optional(),
  budget: TaskBudgetSchema.optional(),
  notificationPolicy: z.enum(INVESTIGATION_NOTIFICATION_POLICIES).optional(),
  nextRunAt: timestamp.optional(),
});

export const InvestigationScheduleUpdateInputSchema = z.strictObject({
  scheduleId: OPAQUE_ID,
  status: z.enum(INVESTIGATION_SCHEDULE_STATUSES).optional(),
  intervalSeconds: z.number().int().positive().max(INVESTIGATION_LIMITS.maxObservationDurationSeconds).optional(),
  cooldownSeconds: z.number().int().nonnegative().max(INVESTIGATION_LIMITS.maxObservationDurationSeconds).optional(),
  dedupeWindowSeconds: z.number().int().positive().max(INVESTIGATION_LIMITS.maxObservationDurationSeconds).optional(),
  maxConcurrentRuns: z.number().int().positive().max(16).optional(),
  budget: TaskBudgetSchema.optional(),
  notificationPolicy: z.enum(INVESTIGATION_NOTIFICATION_POLICIES).optional(),
  nextRunAt: timestamp.optional(),
  baselineRunId: OPAQUE_ID.nullable().optional(),
});

export const InvestigationScheduleListResponseSchema = z.strictObject({
  schedules: z.array(InvestigationScheduleSchema),
});

export const InvestigationScheduleRunListResponseSchema = z.strictObject({
  runs: z.array(InvestigationScheduleRunSchema),
});

export const InvestigationScheduleTickInputSchema = z.strictObject({
  now: timestamp.optional(),
  limit: z.number().int().min(1).max(100).optional(),
});

export const InvestigationScheduleTickResponseSchema = z.strictObject({
  claimed: z.array(InvestigationScheduleRunSchema),
  skipped: z.array(InvestigationScheduleRunSchema),
});

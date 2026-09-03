/**
 * Runtime gate for collector output (see `types/collector.ts`).
 *
 * `ServerSnapshot` is the shape Rust returns from `server_snapshot`, so this schema
 * must stay assignable to that type AND to what `crates/collector` will emit. It is
 * strict on purpose: `snapshots` rows are cross-boundary data, and a silent strip of
 * unknown keys would hide a contract drift instead of failing the parse.
 */

import { z } from "zod";

import { HEALTH_STATES } from "../types/collector.js";
import { ServerCapabilitiesSchema } from "./server.js";

export const HealthStateSchema = z.enum(HEALTH_STATES);

const OsInfoSchema = z.strictObject({
  distribution: z.string().min(1),
  version: z.string().min(1),
  hostname: z.string().min(1),
  kernel: z.string().min(1),
  arch: z.string().min(1),
});

const CpuSampleSchema = z.strictObject({
  model: z.string().min(1),
  cores: z.number().int().positive(),
  usagePercent: z.number().min(0).max(100),
  loadAverage: z.tuple([z.number(), z.number(), z.number()]),
});

const MemorySampleSchema = z.strictObject({
  totalBytes: z.number().int().nonnegative(),
  usedBytes: z.number().int().nonnegative(),
  availableBytes: z.number().int().nonnegative(),
  usagePercent: z.number().min(0).max(100),
});

export const DiskUsageSchema = z.strictObject({
  device: z.string().min(1),
  mountPoint: z.string().min(1),
  totalBytes: z.number().int().nonnegative(),
  usedBytes: z.number().int().nonnegative(),
  usagePercent: z.number().min(0).max(100),
});

export const NetworkInterfaceSampleSchema = z.strictObject({
  name: z.string().min(1),
  rxBytes: z.number().int().nonnegative(),
  txBytes: z.number().int().nonnegative(),
});

export const ContainerInfoSchema = z.strictObject({
  name: z.string().min(1),
  image: z.string().min(1),
  state: z.string().min(1),
  status: z.string().min(1),
  restartCount: z.number().int().nonnegative(),
});

const CollectorSampleSchema = z.strictObject({
  collectorId: z.string().min(1),
  collectedAt: z.string().min(1),
  ok: z.boolean(),
  error: z.string().optional(),
});

/** One row of the `snapshots` table. */
export const ServerSnapshotSchema = z.strictObject({
  id: z.string().min(1),
  /* `srv_` ids only — a snapshot about a prose target is a bug, not data (stable-id rule). */
  serverId: z.string().regex(/^srv_[a-z0-9]+$/, "server id must be an opaque srv_ id"),
  collectedAt: z.string().min(1),
  health: HealthStateSchema,
  os: OsInfoSchema.optional(),
  cpu: CpuSampleSchema.optional(),
  memory: MemorySampleSchema.optional(),
  disks: z.array(DiskUsageSchema).optional(),
  uptimeSeconds: z.number().int().nonnegative().optional(),
  network: z.array(NetworkInterfaceSampleSchema).optional(),
  docker: z
    .strictObject({
      available: z.boolean(),
      containers: z.array(ContainerInfoSchema),
    })
    .optional(),
  capabilities: ServerCapabilitiesSchema,
  collectors: z.array(CollectorSampleSchema).optional(),
});
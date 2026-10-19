/**
 * Runtime gate for collector output (see `types/collector.ts`).
 *
 * `ServerSnapshot` is the shape the host returns from `server_snapshot` *and* what the
 * Python snapshot normalizer emits, so this schema must stay assignable to that type and
 * accept both producers. It is strict on purpose: `snapshots` rows are cross-boundary
 * data, and a silent strip of unknown keys would hide a contract drift instead of failing
 * the parse.
 */

import { z } from "zod";

import { HEALTH_STATES } from "../types/collector.js";
import { SERVER_ID_SCHEMA, ServerCapabilitiesSchema } from "./server.js";

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

// resolv.conf may use a scoped IPv6 resolver such as fe80::53%eth0.
const Ipv6AddressSchema = z.string().min(1).max(256).refine((value) => {
  const parts = value.split("%");
  return parts.length <= 2 && z.ipv6().safeParse(parts[0]).success &&
    (parts.length === 1 || /^[A-Za-z0-9_.:-]{1,64}$/.test(parts[1] ?? ""));
});
const IpAddressSchema = z.union([z.ipv4(), Ipv6AddressSchema]);
const NetworkCidrSchema = z.union([z.cidrv4(), z.cidrv6()]);
const NetworkRouteSchema = z.strictObject({
  destination: z.union([z.literal("default"), NetworkCidrSchema]),
  gateway: IpAddressSchema.optional(),
  interface: z.string().min(1).max(256).optional(),
  metric: z.number().int().nonnegative().max(4_294_967_295).optional(),
  type: z.string().min(1).max(256).optional(),
});

export const NetworkDetailsSchema = z.strictObject({
  interfaces: z.array(z.strictObject({
    name: z.string().min(1).max(256),
    state: z.string().min(1).max(256),
    addresses: z.array(NetworkCidrSchema).max(128),
  })).max(256).optional(),
  ipv4Routes: z.array(NetworkRouteSchema).max(512).optional(),
  ipv6Routes: z.array(NetworkRouteSchema).max(512).optional(),
  dnsServers: z.array(IpAddressSchema).max(32).optional(),
  errors: z.strictObject({
    interfaces: z.string().optional(),
    ipv4Routes: z.string().optional(),
    ipv6Routes: z.string().optional(),
    dnsServers: z.string().optional(),
  }).optional(),
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
  serverId: SERVER_ID_SCHEMA,
  collectedAt: z.string().min(1),
  health: HealthStateSchema,
  os: OsInfoSchema.optional(),
  cpu: CpuSampleSchema.optional(),
  memory: MemorySampleSchema.optional(),
  disks: z.array(DiskUsageSchema).optional(),
  uptimeSeconds: z.number().int().nonnegative().optional(),
  network: z.array(NetworkInterfaceSampleSchema).optional(),
  networkDetails: NetworkDetailsSchema.optional(),
  docker: z
    .strictObject({
      available: z.boolean(),
      containers: z.array(ContainerInfoSchema),
    })
    .optional(),
  capabilities: ServerCapabilitiesSchema,
  collectors: z.array(CollectorSampleSchema).optional(),
});

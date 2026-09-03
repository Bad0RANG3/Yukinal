/**
 * Runtime-validation schemas for cross-boundary data (-R7).
 *
 * Types in `../types/*` are the contract; these schemas are the gate. Anything
 * crossing React <-> Rust <-> agent must be parsed, never cast.
 */

import { z } from "zod";

import { ENVIRONMENTS, RISK_LEVELS } from "../types/risk.js";
import { SERVER_STATUSES } from "../types/server.js";

export const EnvironmentSchema = z.enum(ENVIRONMENTS);
export const RiskLevelSchema = z.enum(RISK_LEVELS);
export const ServerStatusSchema = z.enum(SERVER_STATUSES);

export const ServerCapabilitiesSchema = z.object({
  linux: z.boolean().optional(),
  docker: z.boolean().optional(),
  systemd: z.boolean().optional(),
  nginx: z.boolean().optional(),
  postgres: z.boolean().optional(),
  redis: z.boolean().optional(),
  kubernetes: z.boolean().optional(),
});

export const ServerConnectionSchema = z.object({
  host: z.string().min(1),
  /** 0 is not a port. Empty port defaults are a classic config bug. */
  port: z.number().int().min(1).max(65535),
  username: z.string().min(1),
  identityId: z.string().min(1).optional(),
});

export const ServerMetadataSchema = z.object({
  environment: EnvironmentSchema,
  region: z.string().optional(),
  hostname: z.string().optional(),
  os: z.string().optional(),
  tags: z.array(z.string()).optional(),
  workspaceIds: z.array(z.string()).optional(),
});

export const ServerSchema = z.object({
  /** `srv_` prefixed opaque id; never derived from host/port. */
  id: z.string().regex(/^srv_[a-z0-9]+$/, "server id must be an opaque srv_ id"),
  name: z.string().min(1),
  connection: ServerConnectionSchema,
  groupId: z.string().optional(),
  capabilities: ServerCapabilitiesSchema,
  status: ServerStatusSchema,
  metadata: ServerMetadataSchema,
  createdAt: z.string(),
  updatedAt: z.string(),
});

/** Payload of the "Add Server" form. Secrets are dropped into the keychain here. */
export const AddServerInputSchema = z.object({
  name: z.string().min(1),
  host: z.string().min(1),
  port: z.number().int().min(1).max(65535).optional(),
  username: z.string().min(1),
  environment: EnvironmentSchema,
  groupId: z.string().optional(),
  authentication: z.discriminatedUnion("method", [
    z.object({ method: z.literal("password"), password: z.string().min(1) }),
    z.object({
      method: z.literal("privateKey"),
      privateKeyPem: z.string().min(1),
      passphrase: z.string().min(1).optional(),
    }),
    z.object({ method: z.literal("identity"), identityId: z.string().min(1) }),
  ]),
});

export const ToolTargetSchema = z.object({
  host: z.enum(["local", "remote"]),
  serverId: z.string().optional(),
  workspaceId: z.string().optional(),
  environment: EnvironmentSchema,
});

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

export const ServerCapabilitiesSchema = z.strictObject({
  linux: z.boolean().optional(),
  docker: z.boolean().optional(),
  systemd: z.boolean().optional(),
  nginx: z.boolean().optional(),
  postgres: z.boolean().optional(),
  redis: z.boolean().optional(),
  kubernetes: z.boolean().optional(),
});

export const ServerConnectionSchema = z.strictObject({
  host: z.string().trim().min(1).max(256),
  /** 0 is not a port. Empty port defaults are a classic config bug. */
  port: z.number().int().min(1).max(65535),
  username: z.string().trim().min(1).max(256),
  identityId: z.string().trim().min(1).max(256).optional(),
});

export const ServerMetadataSchema = z.strictObject({
  environment: EnvironmentSchema,
  region: z.string().trim().max(256).optional(),
  hostname: z.string().trim().max(256).optional(),
  os: z.string().trim().max(256).optional(),
  tags: z.array(z.string().trim().max(128)).max(64).optional(),
  workspaceIds: z.array(z.string().trim().max(256)).max(128).optional(),
});

export const ServerSchema = z.strictObject({
  /** `srv_` prefixed opaque id; never derived from host/port. */
  id: z.string().trim().min(1).max(256).regex(/^srv_[a-z0-9]+$/, "server id must be an opaque srv_ id"),
  name: z.string().trim().min(1).max(256),
  connection: ServerConnectionSchema,
  groupId: z.string().trim().max(256).optional(),
  capabilities: ServerCapabilitiesSchema,
  status: ServerStatusSchema,
  metadata: ServerMetadataSchema,
  createdAt: z.string().min(1).max(80),
  updatedAt: z.string().min(1).max(80),
});

export const WorkspaceRepositorySchema = z.strictObject({
  id: z.string().trim().min(1).max(256),
  name: z.string().trim().min(1).max(256),
  host: z.enum(["local", "remote"]),
  path: z.string().min(1).optional(),
  serverId: z.string().min(1).optional(),
  gitUrl: z.string().min(1).optional(),
  defaultBranch: z.string().min(1).optional(),
});

export const WorkspaceSchema = z.strictObject({
  id: z.string().min(1),
  name: z.string().min(1),
  serverIds: z.array(z.string().trim().min(1).max(256)).max(128),
  repositories: z.array(WorkspaceRepositorySchema),
  providerIds: z.array(z.string().trim().min(1).max(256)).max(128),
  defaultEnvironment: EnvironmentSchema,
});

export const WorkspaceListResponseSchema = z.strictObject({
  workspaces: z.array(WorkspaceSchema),
});

/** Payload of the "Add Server" form. Secrets are dropped into the keychain here. */
export const AddServerInputSchema = z.strictObject({
  name: z.string().trim().min(1).max(256),
  host: z.string().trim().min(1).max(256),
  port: z.number().int().min(1).max(65535).optional(),
  username: z.string().trim().min(1).max(256),
  environment: EnvironmentSchema,
  groupId: z.string().trim().max(256).optional(),
  authentication: z.discriminatedUnion("method", [
    z.strictObject({ method: z.literal("password"), password: z.string().min(1).max(4_096) }),
    z.strictObject({
      method: z.literal("privateKey"),
      privateKeyPem: z.string().min(1).max(1_000_000),
      passphrase: z.string().min(1).max(4_096).optional(),
    }),
    z.strictObject({ method: z.literal("identity"), identityId: z.string().trim().min(1).max(256) }),
  ]),
});

export const UpdateServerInputSchema = z.strictObject({
  serverId: z.string().trim().min(1).max(256).regex(/^srv_[a-z0-9]+$/),
  name: z.string().trim().min(1).max(256),
  host: z.string().trim().min(1).max(256),
  port: z.number().int().min(1).max(65535).optional(),
  username: z.string().trim().min(1).max(256),
  environment: EnvironmentSchema,
  groupId: z.string().trim().max(256).optional(),
  authentication: AddServerInputSchema.shape.authentication.optional(),
});

export const ToolTargetSchema = z.strictObject({
  host: z.enum(["local", "remote"]),
  serverId: z.string().trim().min(1).max(256).regex(/^srv_[a-z0-9]+$/).optional(),
  workspaceId: z.string().trim().min(1).max(256).optional(),
  environment: EnvironmentSchema,
}).superRefine((target, context) => {
  if (target.host === "remote" && !target.serverId) {
    context.addIssue({ code: "custom", path: ["serverId"], message: "remote targets require serverId" });
  }
  if (target.host === "local" && target.serverId) {
    context.addIssue({ code: "custom", path: ["serverId"], message: "local targets cannot include serverId" });
  }
});

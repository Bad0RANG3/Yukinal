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

export const HostCertificateAuthoritySchema = z.strictObject({
  caPublicKey: z.string().trim().min(1).max(16_384),
  principals: z
    .array(
      z
        .string()
        .trim()
        .min(1)
        .max(253)
        .refine(
          (value) => !/[\u0000-\u001f\u007f]/.test(value),
          "principal must not contain control characters",
        ),
    )
    .min(1)
    .max(32),
  revocationListPath: z.string().trim().min(1).max(4_096).optional(),
  revocationListUrl: z.string().trim().min(1).max(2_048).optional(),
  revocationListSigners: z
    .array(z.string().trim().min(1).max(16_384))
    .max(8)
    .optional(),
}).superRefine((authority, context) => {
  if (authority.revocationListPath && authority.revocationListUrl) {
    context.addIssue({
      code: "custom",
      message: "revocationListPath and revocationListUrl are mutually exclusive",
      path: ["revocationListUrl"],
    });
  }
});

export const ServerConnectionSchema = z.strictObject({
  host: z.string().trim().min(1).max(256),
  /** 0 is not a port. Empty port defaults are a classic config bug. */
  port: z.number().int().min(1).max(65535),
  username: z.string().trim().min(1).max(256),
  identityId: z.string().trim().min(1).max(256).optional(),
  hostCertificateAuthority: HostCertificateAuthoritySchema.optional(),
});

export const ServerMetadataSchema = z.strictObject({
  environment: EnvironmentSchema,
  region: z.string().trim().max(256).optional(),
  hostname: z.string().trim().max(256).optional(),
  os: z.string().trim().max(256).optional(),
  tags: z.array(z.string().trim().max(128)).max(64).optional(),
  workspaceIds: z.array(z.string().trim().max(256)).max(128).optional(),
});

/**
 * 不透明的 `srv_` 服务器 id —— 全项目**唯一**的定义。
 *
 * 这条正则原名在五个地方各写了一遍（本文件三处、`collector.ts`、`permission.ts`），
 * 而 `ipc.ts` 还导出过第六份等价的 `IpcServerIdSchema`。六份写法已经不一致了：
 * 三份带 `"server id must be an opaque srv_ id"` 这条提示，另三份不带；
 * `UpdateServerInputSchema.serverId` 那份连 trim/长度上限都没有，
 * 于是同一个 id 在「新增」和「更新」两个请求里受两套规则约束。
 *
 * 提示语里的 `server id must be …` 是写给**读日志的人**的：id 校验失败往往出现在
 * 前端拿到了一个被截断或拼错的字符串之后，那时能一眼看出「这应该是个 srv_ 开头的
 * 不透明 id」比什么都重要，所以保留带提示的那份写法。
 */
export const SERVER_ID_SCHEMA = z
  .string()
  .trim()
  .min(1)
  .max(256)
  .regex(/^srv_[a-z0-9]+$/, "server id must be an opaque srv_ id");

export const ServerSchema = z.strictObject({
  id: SERVER_ID_SCHEMA,
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
  hostCertificateAuthority: HostCertificateAuthoritySchema.optional(),
  authentication: z.discriminatedUnion("method", [
    z.strictObject({ method: z.literal("password"), password: z.string().min(1).max(4_096) }),
    /**
     * `passphrase` 是**可选**的，且 `min(1)`：空串不是「空口令」而是「没填」，
     * 表单必须省略字段而不是送一个 `""`（Rust 侧 `min(1).max(4_096)` 与之对齐）。
     * 无口令的明文 key 就是不带这个字段的形状。
     */
    z.strictObject({
      method: z.literal("privateKey"),
      privateKeyPem: z.string().min(1).max(1_000_000),
      passphrase: z.string().min(1).max(4_096).optional(),
    }),
    z.strictObject({
      method: z.literal("certificate"),
      privateKeyPem: z.string().min(1).max(1_000_000),
      passphrase: z.string().min(1).max(4_096).optional(),
      certificatePath: z.string().trim().min(1).max(4_096),
      privateKeyPath: z.string().trim().min(1).max(4_096).optional(),
    }),
    /**
     * ssh-agent：**没有任何 secret 字段**（`.strictObject` 会拒绝多余的 key）。
     * 身份由运行中的 agent 持有，Yukinal 这边连凭据条目都不建。
     */
    z.strictObject({ method: z.literal("agent") }),
    z.strictObject({ method: z.literal("identity"), identityId: z.string().trim().min(1).max(256) }),
  ]),
});

export const UpdateServerInputSchema = z
  .strictObject({
    serverId: SERVER_ID_SCHEMA,
    name: z.string().trim().min(1).max(256),
    host: z.string().trim().min(1).max(256),
    port: z.number().int().min(1).max(65535).optional(),
    username: z.string().trim().min(1).max(256),
    environment: EnvironmentSchema,
    groupId: z.string().trim().max(256).optional(),
    hostCertificateAuthority: HostCertificateAuthoritySchema.optional(),
    clearHostCertificateAuthority: z.boolean().optional(),
    authentication: AddServerInputSchema.shape.authentication.optional(),
  })
  .refine(
    (input) => !(input.hostCertificateAuthority && input.clearHostCertificateAuthority),
    {
      message: "hostCertificateAuthority and clearHostCertificateAuthority are mutually exclusive",
      path: ["hostCertificateAuthority"],
    },
  );

export const ToolTargetSchema = z.strictObject({
  host: z.enum(["local", "remote"]),
  serverId: SERVER_ID_SCHEMA.optional(),
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

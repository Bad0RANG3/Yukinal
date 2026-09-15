/** 出站网络设置（ADR 0022）的运行时契约：命令的入参与响应都从这里来。 */

import { z } from "zod";

import type {
  NetworkProxyResolution,
  NetworkProxySaveInput,
  NetworkProxyView,
} from "../types/network.js";

export const NetworkProxyModeSchema = z.enum(["direct", "system"]);

export const NetworkProxyResolutionSchema = z.discriminatedUnion("kind", [
  z.strictObject({ kind: z.literal("direct") }),
  z.strictObject({
    kind: z.literal("proxy"),
    url: z.string().min(1).max(2_048),
    source: z.string().min(1).max(64),
    hasNoProxy: z.boolean(),
  }),
  z.strictObject({
    kind: z.literal("unusable"),
    reason: z.string().min(1).max(1_024),
  }),
]) satisfies z.ZodType<NetworkProxyResolution>;

export const NetworkProxyViewSchema = z.strictObject({
  mode: NetworkProxyModeSchema,
  hasCredential: z.boolean(),
  resolution: NetworkProxyResolutionSchema,
}) satisfies z.ZodType<NetworkProxyView>;

export const NetworkProxySaveInputSchema = z
  .strictObject({
    mode: NetworkProxyModeSchema,
    // 写死上限：代理凭据与别的凭据同一条规则，超过就不收，而不是截断。
    credential: z.string().min(1).max(1_024).optional(),
    clearCredential: z.boolean().optional(),
  })
  .superRefine((input, context) => {
    if (input.credential !== undefined && input.clearCredential === true) {
      context.addIssue({
        code: "custom",
        path: ["clearCredential"],
        message: "credential and clearCredential are mutually exclusive",
      });
    }
    if (input.mode === "direct" && input.credential !== undefined) {
      context.addIssue({
        code: "custom",
        path: ["credential"],
        message: "a new proxy credential requires system-proxy mode",
      });
    }
  }) satisfies z.ZodType<NetworkProxySaveInput>;

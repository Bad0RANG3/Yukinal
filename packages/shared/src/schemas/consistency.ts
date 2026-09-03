/**
 * Compile-time contract tests: the zod schemas (runtime gate) and the hand-written
 * types (source of truth mirrored by Rust/serde) must stay structurally compatible.
 * If a Rust struct, a TS type or a schema drifts, this file stops compiling.
 */

import type { z } from "zod";

import type { AgentRunRequest, ApprovalResponse } from "../types/chat.js";
import type { AddServerInput, Server } from "../types/server.js";
import type { ToolDeclaration } from "../types/tool.js";
import type { PermissionDecision } from "../types/risk.js";
import type { AddServerInputSchema, ServerSchema } from "./server.js";
import type {
  AgentRunRequestSchema,
  ApprovalResponseSchema,
  PermissionDecisionSchema,
  ToolDeclarationSchema,
} from "./permission.js";

/** Succeeds only when the schema's output is assignable to the declared contract type. */
type Expect<T extends true> = T;
type Assignable<S extends z.ZodType, Target> = z.output<S> extends Target ? true : false;

export type _ServerContract = Expect<Assignable<typeof ServerSchema, Server>>;
export type _AddServerContract = Expect<Assignable<typeof AddServerInputSchema, AddServerInput>>;
export type _PermissionDecisionContract = Expect<Assignable<typeof PermissionDecisionSchema, PermissionDecision>>;
export type _ToolDeclarationContract = Expect<Assignable<typeof ToolDeclarationSchema, ToolDeclaration>>;
export type _ApprovalResponseContract = Expect<Assignable<typeof ApprovalResponseSchema, ApprovalResponse>>;
export type _AgentRunContract = Expect<Assignable<typeof AgentRunRequestSchema, AgentRunRequest>>;

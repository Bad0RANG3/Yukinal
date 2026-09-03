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
import type { IpcCommandMap, IpcCommandName } from "../ipc/index.js";
import type { AddServerInputSchema, ServerSchema } from "./server.js";
import type {
  AgentRunRequestSchema,
  ApprovalResponseSchema,
  PermissionDecisionSchema,
  ToolDeclarationSchema,
} from "./permission.js";
import type { IPC_SCHEMAS } from "./ipc.js";

/** Succeeds only when the schema's output is assignable to the declared contract type. */
type Expect<T extends true> = T;
type Assignable<S extends z.ZodType, Target> = z.output<S> extends Target ? true : false;

export type _ServerContract = Expect<Assignable<typeof ServerSchema, Server>>;
export type _AddServerContract = Expect<Assignable<typeof AddServerInputSchema, AddServerInput>>;
export type _PermissionDecisionContract = Expect<Assignable<typeof PermissionDecisionSchema, PermissionDecision>>;
export type _ToolDeclarationContract = Expect<Assignable<typeof ToolDeclarationSchema, ToolDeclaration>>;
export type _ApprovalResponseContract = Expect<Assignable<typeof ApprovalResponseSchema, ApprovalResponse>>;
export type _AgentRunContract = Expect<Assignable<typeof AgentRunRequestSchema, AgentRunRequest>>;

/**
 * Per-command IPC gates: every command's params and response schema must produce a
 * shape assignable to its `IpcCommandMap` entry. Each check is written out per
 * command (not mapped over a generic key): TypeScript defers a conditional type
 * over a generic, which would silently weaken the `Expect` constraint instead of
 * failing the build. Explicit lines keep one failing command → one error.
 */

type ParamsOf<C extends IpcCommandName> = (typeof IPC_SCHEMAS)[C]["params"];
type ResponseOf<C extends IpcCommandName> = (typeof IPC_SCHEMAS)[C]["response"];

export type _IpcParamsContracts = {
  core_ping: Expect<Assignable<ParamsOf<"core_ping">, IpcCommandMap["core_ping"]["params"]>>;
  server_list: Expect<Assignable<ParamsOf<"server_list">, IpcCommandMap["server_list"]["params"]>>;
  server_add: Expect<Assignable<ParamsOf<"server_add">, IpcCommandMap["server_add"]["params"]>>;
  server_connect: Expect<Assignable<ParamsOf<"server_connect">, IpcCommandMap["server_connect"]["params"]>>;
  server_disconnect: Expect<Assignable<ParamsOf<"server_disconnect">, IpcCommandMap["server_disconnect"]["params"]>>;
  server_snapshot: Expect<Assignable<ParamsOf<"server_snapshot">, IpcCommandMap["server_snapshot"]["params"]>>;
  terminal_open: Expect<Assignable<ParamsOf<"terminal_open">, IpcCommandMap["terminal_open"]["params"]>>;
  terminal_write: Expect<Assignable<ParamsOf<"terminal_write">, IpcCommandMap["terminal_write"]["params"]>>;
  terminal_resize: Expect<Assignable<ParamsOf<"terminal_resize">, IpcCommandMap["terminal_resize"]["params"]>>;
  terminal_close: Expect<Assignable<ParamsOf<"terminal_close">, IpcCommandMap["terminal_close"]["params"]>>;
  agent_spawn: Expect<Assignable<ParamsOf<"agent_spawn">, IpcCommandMap["agent_spawn"]["params"]>>;
  agent_kill: Expect<Assignable<ParamsOf<"agent_kill">, IpcCommandMap["agent_kill"]["params"]>>;
  agent_status: Expect<Assignable<ParamsOf<"agent_status">, IpcCommandMap["agent_status"]["params"]>>;
  agent_logs: Expect<Assignable<ParamsOf<"agent_logs">, IpcCommandMap["agent_logs"]["params"]>>;
  agent_run_start: Expect<Assignable<ParamsOf<"agent_run_start">, IpcCommandMap["agent_run_start"]["params"]>>;
  agent_run_stop: Expect<Assignable<ParamsOf<"agent_run_stop">, IpcCommandMap["agent_run_stop"]["params"]>>;
  agent_approval_respond: Expect<Assignable<ParamsOf<"agent_approval_respond">, IpcCommandMap["agent_approval_respond"]["params"]>>;
  provider_list: Expect<Assignable<ParamsOf<"provider_list">, IpcCommandMap["provider_list"]["params"]>>;
  provider_save_openai: Expect<Assignable<ParamsOf<"provider_save_openai">, IpcCommandMap["provider_save_openai"]["params"]>>;
};

export type _IpcResponseContracts = {
  core_ping: Expect<Assignable<ResponseOf<"core_ping">, IpcCommandMap["core_ping"]["response"]>>;
  server_list: Expect<Assignable<ResponseOf<"server_list">, IpcCommandMap["server_list"]["response"]>>;
  server_add: Expect<Assignable<ResponseOf<"server_add">, IpcCommandMap["server_add"]["response"]>>;
  server_connect: Expect<Assignable<ResponseOf<"server_connect">, IpcCommandMap["server_connect"]["response"]>>;
  server_disconnect: Expect<Assignable<ResponseOf<"server_disconnect">, IpcCommandMap["server_disconnect"]["response"]>>;
  server_snapshot: Expect<Assignable<ResponseOf<"server_snapshot">, IpcCommandMap["server_snapshot"]["response"]>>;
  terminal_open: Expect<Assignable<ResponseOf<"terminal_open">, IpcCommandMap["terminal_open"]["response"]>>;
  terminal_write: Expect<Assignable<ResponseOf<"terminal_write">, IpcCommandMap["terminal_write"]["response"]>>;
  terminal_resize: Expect<Assignable<ResponseOf<"terminal_resize">, IpcCommandMap["terminal_resize"]["response"]>>;
  terminal_close: Expect<Assignable<ResponseOf<"terminal_close">, IpcCommandMap["terminal_close"]["response"]>>;
  agent_spawn: Expect<Assignable<ResponseOf<"agent_spawn">, IpcCommandMap["agent_spawn"]["response"]>>;
  agent_kill: Expect<Assignable<ResponseOf<"agent_kill">, IpcCommandMap["agent_kill"]["response"]>>;
  agent_status: Expect<Assignable<ResponseOf<"agent_status">, IpcCommandMap["agent_status"]["response"]>>;
  agent_logs: Expect<Assignable<ResponseOf<"agent_logs">, IpcCommandMap["agent_logs"]["response"]>>;
  agent_run_start: Expect<Assignable<ResponseOf<"agent_run_start">, IpcCommandMap["agent_run_start"]["response"]>>;
  agent_run_stop: Expect<Assignable<ResponseOf<"agent_run_stop">, IpcCommandMap["agent_run_stop"]["response"]>>;
  agent_approval_respond: Expect<Assignable<ResponseOf<"agent_approval_respond">, IpcCommandMap["agent_approval_respond"]["response"]>>;
  provider_list: Expect<Assignable<ResponseOf<"provider_list">, IpcCommandMap["provider_list"]["response"]>>;
  provider_save_openai: Expect<Assignable<ResponseOf<"provider_save_openai">, IpcCommandMap["provider_save_openai"]["response"]>>;
};

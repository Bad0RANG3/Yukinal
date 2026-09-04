/**
 * Compile-time contract tests: the zod schemas (runtime gate) and the hand-written
 * types (source of truth mirrored by Rust/serde) must stay structurally compatible.
 * If a Rust struct, a TS type or a schema drifts, this file stops compiling.
 */

import type { z } from "zod";

import type { AgentRunRequest, ApprovalResponse } from "../types/chat.js";
import type { AddServerInput, Server, UpdateServerInput } from "../types/server.js";
import type { Activity } from "../types/activity.js";
import type { ServerService, ServerServicesResponse } from "../types/service.js";
import type { ServerLogLine, ServerLogsResponse } from "../types/log.js";
import type { ToolDeclaration } from "../types/tool.js";
import type { PermissionDecision } from "../types/risk.js";
import type { IpcCommandMap, IpcCommandName } from "../ipc/index.js";
import type { AddServerInputSchema, ServerSchema, UpdateServerInputSchema } from "./server.js";
import type { ActivitySchema } from "./activity.js";
import type { ServerServiceSchema, ServerServicesResponseSchema } from "./service.js";
import type { ServerLogLineSchema, ServerLogsResponseSchema } from "./log.js";
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
export type _UpdateServerContract = Expect<Assignable<typeof UpdateServerInputSchema, UpdateServerInput>>;
export type _ActivityContract = Expect<Assignable<typeof ActivitySchema, Activity>>;
export type _ServerServiceContract = Expect<Assignable<typeof ServerServiceSchema, ServerService>>;
export type _ServerServicesContract = Expect<Assignable<typeof ServerServicesResponseSchema, ServerServicesResponse>>;
export type _ServerLogLineContract = Expect<Assignable<typeof ServerLogLineSchema, ServerLogLine>>;
export type _ServerLogsContract = Expect<Assignable<typeof ServerLogsResponseSchema, ServerLogsResponse>>;
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
  server_update: Expect<Assignable<ParamsOf<"server_update">, IpcCommandMap["server_update"]["params"]>>;
  server_delete: Expect<Assignable<ParamsOf<"server_delete">, IpcCommandMap["server_delete"]["params"]>>;
  server_connect: Expect<Assignable<ParamsOf<"server_connect">, IpcCommandMap["server_connect"]["params"]>>;
  server_disconnect: Expect<Assignable<ParamsOf<"server_disconnect">, IpcCommandMap["server_disconnect"]["params"]>>;
  server_snapshot: Expect<Assignable<ParamsOf<"server_snapshot">, IpcCommandMap["server_snapshot"]["params"]>>;
  server_services: Expect<Assignable<ParamsOf<"server_services">, IpcCommandMap["server_services"]["params"]>>;
  server_logs: Expect<Assignable<ParamsOf<"server_logs">, IpcCommandMap["server_logs"]["params"]>>;
  remote_file_list: Expect<Assignable<ParamsOf<"remote_file_list">, IpcCommandMap["remote_file_list"]["params"]>>;
  remote_file_read: Expect<Assignable<ParamsOf<"remote_file_read">, IpcCommandMap["remote_file_read"]["params"]>>;
  activity_list: Expect<Assignable<ParamsOf<"activity_list">, IpcCommandMap["activity_list"]["params"]>>;
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
  provider_import_ccswitch: Expect<Assignable<ParamsOf<"provider_import_ccswitch">, IpcCommandMap["provider_import_ccswitch"]["params"]>>;
  provider_import_ccswitch_apply: Expect<Assignable<ParamsOf<"provider_import_ccswitch_apply">, IpcCommandMap["provider_import_ccswitch_apply"]["params"]>>;
  provider_import_codex: Expect<Assignable<ParamsOf<"provider_import_codex">, IpcCommandMap["provider_import_codex"]["params"]>>;
  provider_import_codex_apply: Expect<Assignable<ParamsOf<"provider_import_codex_apply">, IpcCommandMap["provider_import_codex_apply"]["params"]>>;
  provider_activate: Expect<Assignable<ParamsOf<"provider_activate">, IpcCommandMap["provider_activate"]["params"]>>;
  provider_models: Expect<Assignable<ParamsOf<"provider_models">, IpcCommandMap["provider_models"]["params"]>>;
};

export type _IpcResponseContracts = {
  core_ping: Expect<Assignable<ResponseOf<"core_ping">, IpcCommandMap["core_ping"]["response"]>>;
  server_list: Expect<Assignable<ResponseOf<"server_list">, IpcCommandMap["server_list"]["response"]>>;
  server_add: Expect<Assignable<ResponseOf<"server_add">, IpcCommandMap["server_add"]["response"]>>;
  server_update: Expect<Assignable<ResponseOf<"server_update">, IpcCommandMap["server_update"]["response"]>>;
  server_delete: Expect<Assignable<ResponseOf<"server_delete">, IpcCommandMap["server_delete"]["response"]>>;
  server_connect: Expect<Assignable<ResponseOf<"server_connect">, IpcCommandMap["server_connect"]["response"]>>;
  server_disconnect: Expect<Assignable<ResponseOf<"server_disconnect">, IpcCommandMap["server_disconnect"]["response"]>>;
  server_snapshot: Expect<Assignable<ResponseOf<"server_snapshot">, IpcCommandMap["server_snapshot"]["response"]>>;
  server_services: Expect<Assignable<ResponseOf<"server_services">, IpcCommandMap["server_services"]["response"]>>;
  server_logs: Expect<Assignable<ResponseOf<"server_logs">, IpcCommandMap["server_logs"]["response"]>>;
  remote_file_list: Expect<Assignable<ResponseOf<"remote_file_list">, IpcCommandMap["remote_file_list"]["response"]>>;
  remote_file_read: Expect<Assignable<ResponseOf<"remote_file_read">, IpcCommandMap["remote_file_read"]["response"]>>;
  activity_list: Expect<Assignable<ResponseOf<"activity_list">, IpcCommandMap["activity_list"]["response"]>>;
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
  provider_import_ccswitch: Expect<Assignable<ResponseOf<"provider_import_ccswitch">, IpcCommandMap["provider_import_ccswitch"]["response"]>>;
  provider_import_ccswitch_apply: Expect<Assignable<ResponseOf<"provider_import_ccswitch_apply">, IpcCommandMap["provider_import_ccswitch_apply"]["response"]>>;
  provider_import_codex: Expect<Assignable<ResponseOf<"provider_import_codex">, IpcCommandMap["provider_import_codex"]["response"]>>;
  provider_import_codex_apply: Expect<Assignable<ResponseOf<"provider_import_codex_apply">, IpcCommandMap["provider_import_codex_apply"]["response"]>>;
  provider_activate: Expect<Assignable<ResponseOf<"provider_activate">, IpcCommandMap["provider_activate"]["response"]>>;
  provider_models: Expect<Assignable<ResponseOf<"provider_models">, IpcCommandMap["provider_models"]["response"]>>;
};

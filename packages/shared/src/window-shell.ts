import { z } from "zod";
import { SERVER_STATUSES } from "./types/server.js";
import { SERVER_ID_SCHEMA } from "./schemas/server.js";

/** Presentation only: no credentials, prompts, approvals or execution authority. */
export const WindowShellContextSchema = z.object({
  section: z.enum(["servers", "projects", "tasks", "activity", "settings"]),
  // Match Win32's UTF-16 title bound, including astral characters.
  serverName: z.string().max(160).refine((name) => name.length <= 160).regex(/^[^\u0000-\u001f\u007f\p{Surrogate}]*$/u).nullable(),
  serverStatus: z.enum(SERVER_STATUSES).nullable(),
  agentBusy: z.boolean(),
}).strict();

export type WindowShellContext = z.infer<typeof WindowShellContextSchema>;

const ShellText = (max: number) => z.string().max(max)
  .refine((value) => value.length <= max)
  .regex(/^[^\u0000-\u001f\u007f\p{Surrogate}]*$/u);

export const WindowShellSidebarSchema = z.object({
  selectedServerId: SERVER_ID_SCHEMA.nullable(),
  servers: z.array(z.object({
    id: SERVER_ID_SCHEMA,
    name: ShellText(160),
    detail: ShellText(256),
    status: z.enum(SERVER_STATUSES),
  }).strict()),
  loading: z.boolean(),
  failed: z.boolean(),
  blocked: z.boolean(),
}).strict().refine((value) => new Set(value.servers.map((server) => server.id)).size === value.servers.length);
export type WindowShellSidebar = z.infer<typeof WindowShellSidebarSchema>;

// UI intent only. The consumer resolves current IDs and existing application services.
export const WindowShellActionSchema = z.discriminatedUnion("type", [
  z.object({ type: z.literal("navigate"), section: WindowShellContextSchema.shape.section }).strict(),
  z.object({ type: z.enum(["add-server", "refresh-servers"]) }).strict(),
  z.object({ type: z.enum(["select-server", "edit-server", "connect-server", "disconnect-server", "delete-server"]), serverId: SERVER_ID_SCHEMA }).strict(),
]);
export type WindowShellAction = z.infer<typeof WindowShellActionSchema>;

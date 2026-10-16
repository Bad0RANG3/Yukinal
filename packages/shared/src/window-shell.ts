import { z } from "zod";
import { SERVER_STATUSES } from "./types/server.js";

/** Presentation only: no credentials, prompts, approvals or execution authority. */
export const WindowShellContextSchema = z.object({
  section: z.enum(["servers", "projects", "tasks", "activity", "settings"]),
  // Match Win32's UTF-16 title bound, including astral characters.
  serverName: z.string().max(160).refine((name) => name.length <= 160).regex(/^[^\u0000-\u001f\u007f\p{Surrogate}]*$/u).nullable(),
  serverStatus: z.enum(SERVER_STATUSES).nullable(),
  agentBusy: z.boolean(),
}).strict();

export type WindowShellContext = z.infer<typeof WindowShellContextSchema>;

import { z } from "zod";
import { SERVER_STATUSES } from "./types/server.js";

/** Presentation only: no credentials, prompts, approvals or execution authority. */
export const WindowShellContextSchema = z.object({
  section: z.enum(["servers", "projects", "tasks", "activity", "settings"]),
  // Match Win32 UTF-16 bounds and reject lone surrogate code points.
  serverName: z.string().max(160).refine((name) => name.length <= 160)
    .regex(/^[^\u0000-\u001f\u007f\p{Surrogate}]*$/u).nullable(),
  serverStatus: z.enum(SERVER_STATUSES).nullable(),
  agentBusy: z.boolean(),
}).strict();

export type WindowShellContext = z.infer<typeof WindowShellContextSchema>;

/** The visible AI surface continues through native chrome; CSS pixels, no actions. */
export const WindowShellSurfaceSchema = z.object({
  viewportWidth: z.number().finite().positive().max(32_768),
  left: z.number().finite().nonnegative().max(32_768),
  right: z.number().finite().positive().max(32_768),
  radius: z.number().finite().nonnegative().max(64),
  inspectorLeft: z.number().finite().nonnegative().max(32_768),
  background: z.string().regex(/^#[0-9a-f]{6}$/iu),
  border: z.string().regex(/^#[0-9a-f]{6}$/iu),
}).strict().refine((surface) => surface.left < surface.right && surface.right <= surface.inspectorLeft && surface.inspectorLeft <= surface.viewportWidth,
  "Surface must fit inside the viewport").nullable();

export type WindowShellSurface = z.infer<typeof WindowShellSurfaceSchema>;

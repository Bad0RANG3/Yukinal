import { z } from "zod";

const DockerContainerRefSchema = z.string().regex(/^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/, "invalid Docker container reference");

export const DockerLogsInputSchema = z.strictObject({
  container: DockerContainerRefSchema,
  tail: z.number().int().min(1).max(500).optional(),
});

export const DockerLogsResultSchema = z.strictObject({
  container: DockerContainerRefSchema,
  lines: z.array(z.string()).max(500),
  truncated: z.boolean(),
});

export const DockerInspectInputSchema = z.strictObject({
  container: DockerContainerRefSchema,
});

export const DockerInspectResultSchema = z.strictObject({
  id: z.string().min(1),
  name: DockerContainerRefSchema,
  image: z.string().min(1),
  state: z.string().min(1),
  status: z.string().min(1),
  restartCount: z.number().int().nonnegative(),
  startedAt: z.string().min(1).optional(),
  health: z.string().min(1).optional(),
});

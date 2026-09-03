/**
 * `system.echo` — the first tool in the registry.
 *
 * Purpose is not functionality: it proves the whole chain
 *   declaration -> provider name mapping -> permission -> validation -> timeout -> trace
 * works before any real SSH/Docker tool exists.
 */

import { z } from "zod";

import type { Tool } from "../tool.js";

const inputSchema = z.object({
  message: z.string().min(1).max(500),
});

export const systemEchoTool: Tool<z.infer<typeof inputSchema>, { message: string; at: string; host: string }> = {
  name: "system.echo",
  description:
    "Echo a string back from the agent runtime. Read-only, no environment access. Used to verify the tool pipeline end to end.",
  risk: "read",
  timeoutMs: 2_000,
  cancellable: true,
  retry: { maxAttempts: 1, backoffMs: 0 },
  input: inputSchema,
  async execute(input, context) {
    context.log("echoing", { length: input.message.length });
    return {
      message: input.message,
      at: new Date().toISOString(),
      host: context.target.host,
    };
  },
};

import { z } from "zod";

export const ChatMessageRoleSchema = z.enum(["user", "assistant", "tool", "system"]);

export const ChatSessionSchema = z.strictObject({
  id: z.string().trim().min(1).max(256),
  workspaceId: z.string().trim().min(1).max(256).optional(),
  serverId: z.string().trim().min(1).max(256).optional(),
  title: z.string().trim().min(1).max(200),
  createdAt: z.string().min(1).max(80),
  updatedAt: z.string().min(1).max(80),
  archivedAt: z.string().min(1).max(80).optional(),
  messageCount: z.number().int().nonnegative(),
  lastMessagePreview: z.string().max(240).optional(),
});

export const ChatMessageSchema = z.strictObject({
  id: z.string().trim().min(1).max(256),
  sessionId: z.string().trim().min(1).max(256),
  role: ChatMessageRoleSchema,
  content: z.string().max(100_000),
  traceId: z.string().trim().min(1).max(256).optional(),
  createdAt: z.string().min(1).max(80),
});

export const ChatSessionListResponseSchema = z.strictObject({
  sessions: z.array(ChatSessionSchema),
});

export const ChatSessionDetailResponseSchema = z.strictObject({
  session: ChatSessionSchema,
  messages: z.array(ChatMessageSchema),
});

export const ChatSessionCreateResponseSchema = z.strictObject({
  session: ChatSessionSchema,
});

export const ChatMessageAppendResponseSchema = z.strictObject({
  message: ChatMessageSchema,
});

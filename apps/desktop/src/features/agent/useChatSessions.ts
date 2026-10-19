/**
 * 对话记录（durable record）：本机数据库里的会话生命周期。
 *
 * 这里刻意不碰实时动态 —— 「正在发生什么」属于 useAgentRun，「记录里有什么」
 * 属于本模块。两者只在 AgentPanel 里被接线，因此恢复一段旧对话不会让
 * 事件流逻辑跟着变复杂，反之亦然。
 */

import {
  IPC_COMMANDS,
  type AgentPromptPart,
  type ChatMessage,
  type ChatSession,
} from "@yukinal/shared";
import { useCallback, useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";

import { errorMessage } from "../../lib/format.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { sessionTitleFromPrompt, type ReplyIdentity } from "./transcript.js";

/** 预览模式没有数据库，但输入框仍要能用；这个 id 永远不会被持久化。 */
const EPHEMERAL_SESSION_ID = "ses_ui";

export type ChatSessions = {
  activeSessionId: string | null;
  activeSession: ChatSession | null;
  title: string;
  serverId: string | null;
  /** 当前会话已归档：可以读，不能继续发消息。 */
  archived: boolean;
  /** 任何一次记录写入失败的原因；面板负责呈现。 */
  error: string | null;
  /** 切到一段全新对话（未落库，首条消息时才创建）。 */
  startNewSession(): void;
  /** 打开一段已存在的对话；调用方负责把 messages 灌进动态。 */
  openStoredSession(session: ChatSession): void;
  /** 确保会话存在并写入这条用户消息，返回真正落库的 sessionId。 */
  recordUserMessage(
    prompt: string,
    messageId: string,
    serverId?: string | null,
    parts?: AgentPromptPart[],
  ): Promise<string>;
  /**
   * 追加一条 Agent 回复。返回 false 表示没有写进任何会话（例如运行中对话被删），
   * 调用方必须把这件事告诉用户，而不是当作成功。
   */
  recordAssistantMessage(content: string, identity?: ReplyIdentity): Promise<boolean>;
  /** 归档状态变化只会影响当前会话。 */
  noteSessionUpdated(session: ChatSession): void;
  /** 返回 true 表示被删除的正是当前会话，调用方需要开一段新对话。 */
  noteSessionDeleted(sessionId: string): boolean;
};

export function useChatSessions(): ChatSessions {
  const shell = isDesktopShell();
  const queryClient = useQueryClient();
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [activeSession, setActiveSession] = useState<ChatSession | null>(null);
  const [title, setTitle] = useState("新任务");
  const [serverId, setServerId] = useState<string | null>(null);
  const archived = useWorkspaceStore((state) => state.agentSessionArchived);
  const setArchived = useWorkspaceStore((state) => state.setAgentSessionArchived);
  const [error, setError] = useState<string | null>(null);
  /**
   * 事件回调只注册一次，因此它必须读到「此刻」的会话 id，而不是注册时的闭包值。
   */
  const activeSessionIdRef = useRef<string | null>(null);

  // Session identity is owned by this mounted panel. Dropping it must also drop
  // the shared archived/history state rather than leaving a stale editor guard.
  useEffect(() => () => {
    useWorkspaceStore.setState({ agentSessionArchived: false, agentHistoryOpen: false });
  }, []);

  const activate = useCallback((sessionId: string | null, nextArchived: boolean): void => {
    activeSessionIdRef.current = sessionId;
    setActiveSessionId(sessionId);
    useWorkspaceStore.setState({ agentSessionArchived: nextArchived, agentHistoryOpen: false });
  }, []);

  const recordMessage = useCallback(
    async (
      sessionId: string | null,
      role: ChatMessage["role"],
      content: string,
      messageId?: string,
      parts?: AgentPromptPart[],
      identity?: ReplyIdentity,
    ): Promise<boolean> => {
      if (!shell || !sessionId || (!content.trim() && !parts?.length)) return false;
      try {
        await callDesktop(IPC_COMMANDS.chatMessageAppend, {
          sessionId,
          messageId,
          role,
          content,
          ...(parts?.length ? { parts } : {}),
          ...identity,
        });
        void queryClient.invalidateQueries({ queryKey: ["agent-chat-sessions"] });
        return true;
      } catch (cause) {
        setError(`对话记录保存失败：${errorMessage(cause)}`);
        return false;
      }
    },
    [shell, queryClient],
  );

  const recordUserMessage = useCallback(
    async (
      prompt: string,
      messageId: string,
      serverId?: string | null,
      parts?: AgentPromptPart[],
    ): Promise<string> => {
      if (!shell) return EPHEMERAL_SESSION_ID;
      let sessionId = activeSessionIdRef.current;
      if (!sessionId) {
        try {
          const image = parts?.find(
            (part) =>
              part.type === "image" ||
              part.type === "file" ||
              part.type === "document",
          );
          const created = await callDesktop(IPC_COMMANDS.chatSessionCreate, {
            title: sessionTitleFromPrompt(prompt || image?.name || "图片输入"),
            serverId: serverId ?? undefined,
          });
          sessionId = created.session.id;
          setActiveSession(created.session);
          setTitle(created.session.title);
          setServerId(created.session.serverId ?? null);
          activate(sessionId, false);
        } catch (cause) {
          setError(`对话记录创建失败：${errorMessage(cause)}`);
          throw new Error(`对话记录创建失败：${errorMessage(cause)}`);
        }
      }
      if (!await recordMessage(sessionId, "user", prompt, messageId, parts)) throw new Error("用户消息未保存，请重试。");
      return sessionId;
    },
    [activate, recordMessage, shell],
  );

  const recordAssistantMessage = useCallback(
    async (content: string, identity?: ReplyIdentity): Promise<boolean> => {
      // In a browser preview there is no session store at all: nothing was lost.
      if (!shell) return true;
      if (!activeSessionIdRef.current) return false;
      return recordMessage(activeSessionIdRef.current, "assistant", content, undefined, undefined, identity);
    },
    [recordMessage, shell],
  );

  const startNewSession = useCallback((): void => {
    activate(null, false);
    setActiveSession(null);
    setTitle("新任务");
    setServerId(null);
    setError(null);
  }, [activate]);

  const openStoredSession = useCallback((session: ChatSession): void => {
    activate(session.id, Boolean(session.archivedAt));
    setActiveSession(session);
    setTitle(session.title);
    setServerId(session.serverId ?? null);
    setError(null);
  }, [activate]);

  const noteSessionUpdated = useCallback((session: ChatSession): void => {
    if (session.id !== activeSessionIdRef.current) return;
    setTitle(session.title);
    setActiveSession(session);
    setArchived(Boolean(session.archivedAt));
  }, [setArchived]);

  const noteSessionDeleted = useCallback((sessionId: string): boolean => {
    if (sessionId !== activeSessionIdRef.current) return false;
    activate(null, false);
    setActiveSession(null);
    setTitle("新任务");
    setServerId(null);
    setError(null);
    return true;
  }, [activate]);

  return {
    activeSessionId,
    activeSession,
    title,
    serverId,
    archived,
    error,
    startNewSession,
    openStoredSession,
    recordUserMessage,
    recordAssistantMessage,
    noteSessionUpdated,
    noteSessionDeleted,
  };
}

/* `errorText` 已删除：它与 `AgentHistoryPane.tsx` 的 `errorMessage` 是同一个表达式。
   统一到 `lib/format.js` 的 `errorMessage`。 */

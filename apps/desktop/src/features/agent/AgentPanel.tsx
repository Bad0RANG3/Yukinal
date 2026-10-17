/**
 * Agent 面板 —— 组合根。
 *
 * 这个文件只回答「这些东西怎么接在一起」：把侧车存活信号、provider 列表、
 * 本地对话记录和事件流连成一条链路，再把结果交给各个展示组件。
 *
 * 真正的机制都已经搬到各自最深的地方：
 *   - 事件流、run 生命周期、审批簿记 → useAgentRun
 *   - 什么被写进本地记录             → useChatSessions
 *   - 有哪些模型可选、选中了哪个     → useAgentModels
 *   - 动态的顺序与文案               → transcript
 *   - 焦点、Escape、收起动画         → useAgentPanelShell
 *
 * 规则：不造假 transcript。没有运行中的 run 就没有消息；Stop 立刻掐断在途请求。
 */

import {
  IPC_COMMANDS,
  type AgentPromptPart,
  type ChatMessage,
  type ChatSession,
  type Environment,
  type RestartRecord,
} from "@yukinal/shared";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";

import { Icon } from "../../components/Icon.js";
import { callDesktop, isDesktopShell, isNativeDesktopShell } from "../../lib/ipc.js";
import { errorMessage } from "../../lib/format.js";
import { useDismissOnOutsidePointer } from "../../hooks/useDismissOnOutsidePointer.js";
import { isPreviewFixture } from "../../lib/preview-fixtures.js";
import { useAgentStatus, useSpawnAgent } from "../../lib/runtime.js";
import { useServers } from "../../lib/servers.js";
import { usePreferencesStore } from "../../stores/preferences-store.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { AgentComposer } from "./AgentComposer.js";
import { AgentFeed } from "./AgentFeed.js";
import { AgentHeader, AgentRunStatus } from "./AgentHeader.js";
import { AgentHistoryPane } from "./AgentHistoryPane.js";
import { AgentRecentConversations } from "./AgentRecentConversations.js";
import { useRecentAgentSessions } from "./useRecentAgentSessions.js";
import { AgentNotices } from "./AgentNotices.js";
import {
  commandHelpText,
  resolveMentionedServer,
  unavailableReason,
  type CommandContext,
  type MentionCandidate,
  type Submission,
} from "./composer-triggers.js";
import { promptPartsWithAttachments } from "./image-attachments.js";
import { entriesFromMessages, lastUserEntry, type Entry } from "./transcript.js";
import { AGENT_TEAMS, resolveAgentRole } from "../agent-nist/agent-nist.js";
import { CoordinatorStatus, CoordinatorTasks } from "./CoordinatorTasks.js";
import { coordinatorConfig, coordinatorScope, coordinatorEntries } from "./coordinator-state.js";
import { useCoordinatorTasks } from "./useCoordinatorTasks.js";
import { ConversationWatchStatus } from "../agent-nist/TeamWatchPane.js";
import { attentionReference } from "../agent-nist/team-operations.js";
import { useTeamRuntimeStore } from "../agent-nist/team-runtime-store.js";
import { useWorkpagesStore } from "../workpages/workpages-store.js";
import { useAgentRoles } from "../agent-nist/useAgentRoles.js";
import { recalledDraft, rememberDraft, type SessionDraft } from "./session-drafts.js";
import { requestPolicyId, type RunPolicyChoice } from "./run-policy.js";
import { useAgentModels } from "./useAgentModels.js";
import { reasoningLevels, requestReasoningEffort } from "./reasoning.js";
import { useAgentPanelShell } from "./useAgentPanelShell.js";
import { useAgentRun } from "./useAgentRun.js";
import { useChatSessions } from "./useChatSessions.js";
import { useFeedFollow } from "./useFeedFollow.js";
import { WorkspaceConversation, WorkspaceLayers } from "../workpages/WorkspaceLayers.js";

/**
 * 把一次自动恢复压成一个可比较的字符串。
 *
 * `attempt` 与 `at` 都要参与：`at` 只精确到秒，同一秒内的两次崩溃会得到相同的
 * 时间戳，只比时间就会漏掉第二次重启——而漏掉它意味着一个已经死掉的 run 继续显示
 * 成「运行中」。
 */
function restartId(restart: RestartRecord | undefined): string | null {
  return restart ? `${restart.attempt}:${restart.at}` : null;
}

export function AgentPanel({ onCloseStart, onCloseEnd, workspace = false, workspaceControls, workspaceWorkpages, onAttentionRequested, onOpenTeam }: {
  onCloseStart?: () => void;
  onCloseEnd?: () => void;
  workspace?: boolean;
  workspaceControls?: ReactNode;
  workspaceWorkpages?: ReactNode;
  onAttentionRequested?: () => void;
  onOpenTeam?: () => void;
}) {
  const agentStatus = useAgentStatus({ poll: true });
  const spawnAgent = useSpawnAgent();
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const selectedTaskId = useWorkspaceStore((state) => state.selectedTaskId);
  const requestedOpen = useWorkspaceStore((state) => state.agentOpen);
  const agentOpen = workspace || requestedOpen;
  const setAgentOpen = useWorkspaceStore((state) => state.setAgentOpen);
  const toggleAgent = useWorkspaceStore((state) => state.toggleAgent);
  const setPrimary = useWorkspaceStore((state) => state.setPrimary);
  const permissionMode = usePreferencesStore((state) => state.agentPermissionMode);
  const runMode = usePreferencesStore((state) => state.agentRunMode);
  const delivery = usePreferencesStore((state) => state.agentDelivery);
  const reasoningEffort = usePreferencesStore((state) => state.agentReasoningEffort);
  const setPreferences = usePreferencesStore((state) => state.setPreferences);

  const shell = isDesktopShell();
  const preview = isPreviewFixture();
  const livePreviewChat = preview && isNativeDesktopShell();
  const agentRunning = (!preview || livePreviewChat) && agentStatus.data?.running === true;
  const servers = useServers();
  const focusServer = servers.data?.find((server) => server.id === selectedServerId);

  const models = useAgentModels();
  const roles = useAgentRoles();
  const selectedRoleId = useWorkspaceStore((state) => state.agentRoleId);
  const contextEpoch = useWorkspaceStore((state) => state.agentContextEpoch);
  const setSelectedRoleId = useWorkspaceStore((state) => state.setAgentRoleId);
  const sessions = useChatSessions();
  const recent = useRecentAgentSessions(selectedServerId, 3);
  // 「收到审批时把面板带到前景」是导航决策，所以由面板注入，而不是让运行模块去读 store。
  const run = useAgentRun({
    sidecarRunning: agentRunning,
    // 崩溃与自动重启之间的间隔可能短于一次轮询，所以「进程死过」这件事要由重启记录
    // 提供，而不能只靠 `running === false`。
    sidecarRestart: restartId(agentStatus.data?.restart),
    onAssistantMessage: (text, identity, sessionId) => void sessions.recordAssistantMessage(text, identity, sessionId),
    onApprovalRequested: () => { setAgentOpen(true); onAttentionRequested?.(); },
  });

  const teamTasks = useCoordinatorTasks(agentStatus.data?.running, () => { setAgentOpen(true); onAttentionRequested?.(); });
  // 草稿和「正在跑」放在 workspace store 里，是因为首次使用引导要先替用户填好
  // 输入框、并避免覆盖正在进行的任务 —— 两者都是跨面板的事实，不只是本组件状态。
  const prompt = useWorkspaceStore((state) => state.agentDraft);
  const setPrompt = useWorkspaceStore((state) => state.setAgentDraft);
  const attachments = useWorkspaceStore((state) => state.agentAttachments);
  const setAttachments = useWorkspaceStore((state) => state.setAgentAttachments);
  const setAgentBusy = useWorkspaceStore((state) => state.setAgentBusy);
  useEffect(() => {
    setAgentBusy(run.running);
    return () => setAgentBusy(false);
  }, [run.running, setAgentBusy]);
  const [runContext, setRunContext] = useState<string | null>(null);
  const historyOpen = useWorkspaceStore((state) => state.agentHistoryOpen);
  const setHistoryOpen = useWorkspaceStore((state) => state.setAgentHistoryOpen);
  /** 记录视图关闭时把焦点还给它 —— 见 AgentHistoryPane 的卸载 effect。 */
  const historyButtonRef = useRef<HTMLButtonElement>(null);
  const historyPopoverRef = useRef<HTMLDivElement>(null);
  useDismissOnOutsidePointer(historyOpen, [historyPopoverRef, historyButtonRef], useCallback(() => setHistoryOpen(false), [setHistoryOpen]));
  const [contextError, setContextError] = useState<string | null>(null);
  const [resettingContext, setResettingContext] = useState(false);
  const [openingSession, setOpeningSession] = useState(false);
  const drafts = useRef(new Map<string, SessionDraft>());
  const scopeViews = useRef(new Map<string | null, { session: ChatSession | null; entries: Entry[] }>());
  const scopeRef = useRef(selectedServerId);
  const liveView = useRef({ prompt, attachments, sessionId: sessions.activeSessionId, session: sessions.activeSession, entries: run.entries, running: run.running });
  liveView.current = { prompt, attachments, sessionId: sessions.activeSessionId, session: sessions.activeSession, entries: run.entries, running: run.running };
  /**
   * 这次运行按哪套策略判定。**有意不写进偏好设置**：偏好会在重启后自动恢复，而一次
   * 放宽审批的策略选择（比如在本机目标上选「开发策略」）如果留到下次启动、用户又没
   * 注意到，就等于悄悄改变了审批边界。运行模式与批准方式是「我愿意长期这么干」的
   * 设定，策略覆盖是「这一次」，所以它只活在面板的这一次会话里。
   */
  const [runPolicy, setRunPolicy] = useState<RunPolicyChoice>(null);
  const follow = useFeedFollow(agentOpen, run.entries);
  // 必须稳定：焦点陷阱那个 effect 以它为依赖，每次渲染都换新函数会让键盘监听反复重装。
  const closePanel = useCallback(() => setAgentOpen(false), [setAgentOpen]);
  const panel = useAgentPanelShell({
    agentOpen,
    close: closePanel,
    togglePanel: toggleAgent,
    onCloseStart,
    onCloseEnd,
    persistent: workspace,
  });

  const canSend = shell && agentRunning && run.listening && models.providerReady && !sessions.archived && !resettingContext && !openingSession;

  const commandContext: CommandContext = { running: run.running, archived: sessions.archived, desktop: shell };

  /** 可被 @ 提及的服务器 —— 直接来自 Rust 的服务器列表，不是凭空的候选。 */
  const mentionCandidates: MentionCandidate[] = useMemo(
    () => (servers.data ?? []).map((server) => ({
      id: server.id,
      label: server.name,
      detail: `${server.connection.username}@${server.connection.host}`,
    })),
    [servers.data],
  );
  const composerMentions: MentionCandidate[] = useMemo(() => [
    ...AGENT_TEAMS.map((team) => ({ id: `team:${team.id}`, label: team.label, aliases: [team.id, `${team.id}-team`, team.englishName.replaceAll(" ", "")], detail: team.description })),
    ...mentionCandidates,
  ], [mentionCandidates]);
  const modelReasoningLevels = reasoningLevels(models.selectedProvider, models.selectedModel ?? models.selectedProvider?.model);

  const rememberCurrentDraft = (): void => rememberDraft(drafts.current, scopeRef.current, liveView.current.sessionId, { prompt: liveView.current.prompt, attachments: liveView.current.attachments });
  const restoreDraft = (serverId: string | null, sessionId: string | null): void => {
    const next = recalledDraft(drafts.current, serverId, sessionId);
    setPrompt(next.prompt);
    setAttachments(next.attachments);
  };
  const switchScope = (serverId: string | null): void => {
    if (scopeRef.current === serverId || run.running) return;
    rememberCurrentDraft();
    scopeViews.current.set(scopeRef.current, { session: sessions.activeSession, entries: run.entries });
    const next = scopeViews.current.get(serverId);
    scopeRef.current = serverId;
    if (next?.session) sessions.openStoredSession(next.session);
    else sessions.startNewSession();
    run.loadTranscript(next?.entries ?? []);
    restoreDraft(serverId, next?.session?.id ?? null);
    setHistoryOpen(false);
    setRunContext(null);
    setContextError(null);
  };
  useEffect(() => { switchScope(selectedServerId); }, [selectedServerId, run.running]);
  const seenContextEpoch = useRef(contextEpoch);
  useEffect(() => {
    if (seenContextEpoch.current === contextEpoch) return;
    seenContextEpoch.current = contextEpoch;
    scopeViews.current.clear();
    drafts.current.clear();
    sessions.startNewSession();
    run.clearTranscript();
    setRunContext(null);
    setContextError(null);
    setHistoryOpen(false);
  }, [contextEpoch]);

  const policyTarget = useMemo(() => {
    const mentioned = resolveMentionedServer(prompt, mentionCandidates);
    const serverId = mentioned?.id ?? selectedServerId;
    return servers.data?.find((server) => server.id === serverId);
  }, [prompt, mentionCandidates, selectedServerId, servers.data]);
  const targetEnvironment: Environment = policyTarget?.metadata.environment ?? "unknown";

  const send = async (
    raw: string,
    sendAttachments: readonly AgentPromptPart[] = attachments,
    consumeDraft = true,
  ): Promise<void> => {
    const text = raw.trim();
    const parts = promptPartsWithAttachments(text, sendAttachments);
    if (!parts.length || !canSend || run.running) return;
    // 提示词里的 @服务器 决定这次运行的目标；没提及时才回落到当前选中的服务器。
    const mentioned = resolveMentionedServer(text, mentionCandidates);
    const focusServerId = coordinatorScope(text, mentionCandidates, selectedServerId);
    const requestedRole = resolveAgentRole(text, roles.roles, selectedRoleId);
    const coordination = coordinatorConfig(roles.roles, models.providers.data ?? [], preview ? [] : servers.data ?? [], requestedRole.source === "auto" ? null : requestedRole.role.id);
    if (!coordination.roles.length) {
      setContextError("没有可用的团队模型，请检查团队中的 AI 连接绑定。");
      return;
    }
    const targetView = scopeViews.current.get(focusServerId);
    const targetDraft = recalledDraft(drafts.current, focusServerId, targetView?.session?.id ?? null);
    if (focusServerId !== scopeRef.current && (targetDraft.prompt.length || targetDraft.attachments.length)) {
      setContextError("目标服务器还有未发送的草稿，请先切换并处理。");
      return;
    }
    if (focusServerId !== scopeRef.current && targetView?.session?.archivedAt) {
      setContextError("目标服务器的当前对话已归档，请先恢复或新建任务。");
      return;
    }
    const originScope = scopeRef.current;
    const originSessionId = sessions.activeSessionId;
    if (focusServerId !== originScope) {
      switchScope(focusServerId);
      useWorkspaceStore.getState().selectServer(focusServerId);
    }
    setContextError(null);
    setRunContext(mentioned?.label ?? focusServer?.name ?? "全局工作区");
    follow.pinToBottom();
    if (consumeDraft) {
      if (focusServerId !== originScope) rememberDraft(drafts.current, originScope, originSessionId, { prompt: "", attachments: [] });
      setPrompt("");
      setAttachments([]);
    }
    const workspaceState = useWorkspaceStore.getState();
    const pageState = useWorkpagesStore.getState();
    const activePage = !pageState.collapsed ? pageState.pages.find((page) => page.id === pageState.activeId) : undefined;
    const attention = useTeamRuntimeStore.getState().attention;
    const uiContext = attentionReference({ section: workspaceState.primary, serverId: workspaceState.selectedServerId, serverView: workspaceState.primary === "servers" ? workspaceState.serverPage : undefined, workpage: activePage ? { kind: activePage.kind, serverId: activePage.serverId } : undefined, action: attention && Date.now() - attention.at < 120_000 && (!attention.serverId || attention.serverId === focusServerId) ? attention.action : undefined });
    const outcome = await run.start({
      prompt: text,
      persona: { id: "coordinator", label: "协调助手", systemPrompt: "用户始终与协调助手沟通；团队在后台完成独立任务。" },
      coordination,
      team: "white",
      uiContext,
      participant: { id: "coordinator", label: "协调助手", kind: "agent" },
      taskId: preview ? null : selectedTaskId,
      parts,
      persistUserMessage: (messageId, promptParts) =>
        sessions.recordUserMessage(text, messageId, preview ? null : focusServerId, promptParts),
      providerId: models.selectedProviderId,
      model: models.selectedModel ?? models.selectedProvider?.model,
      reasoningEffort: requestReasoningEffort(models.selectedProvider, models.selectedModel ?? models.selectedProvider?.model, reasoningEffort),
      focusServerId: preview ? null : focusServerId,
      toolAccess: preview ? "none" : undefined,
      permissionMode,
      mode: runMode,
      // 「按环境自动」在这里就是「不带这个字段」，由 requestPolicyId 保证。
      policyId: requestPolicyId(runPolicy),
      delivery,
    });
    // 没跑起来就把输入还给用户，别让他重新打一遍。
    if (outcome !== "started" && consumeDraft) {
      setPrompt(text);
      setAttachments([...sendAttachments]);
    }
  };

  /**
   * 提交的唯一去处。命令与提问在这里分流，而且未知/不可用的命令一律给出
   * 明确反馈 —— 绝不退化成普通提问发出去，否则一个笔误就可能变成一次真实的
   * 远端操作。
   */
  const handleSubmit = (submission: Submission): void => {
    if (submission.kind === "prompt") {
      void send(submission.text);
      return;
    }
    if (submission.kind === "unknown") {
      setPrompt("");
      run.appendSystemEntry(`没有名为 /${submission.name} 的命令。输入 /help 查看可用命令。`);
      return;
    }
    const reason = unavailableReason(submission.spec, commandContext);
    if (reason) {
      setPrompt("");
      run.appendSystemEntry(`/${submission.spec.name} 现在不可用：${reason}`);
      return;
    }
    setPrompt("");
    switch (submission.spec.name) {
      case "new":
        startNewSession(false);
        return;
      case "clear":
        run.clearTranscript();
        return;
      case "history":
        setHistoryOpen(true);
        return;
      case "stop":
        void run.stop();
        return;
      case "help":
        run.appendSystemEntry(commandHelpText(commandContext));
        return;
      default:
        // 有可用性声明却没有对应实现 —— 必须报出来，而不是假装成功。
        run.appendSystemEntry(`命令 /${submission.spec.name} 尚未接入。`);
    }
  };

  const startNewSession = (preserveDraft = true): void => {
    if (run.running) return;
    if (sessions.activeSessionId && preserveDraft) rememberCurrentDraft();
    sessions.startNewSession();
    setHistoryOpen(false);
    setRunContext(null);
    if (sessions.activeSessionId) restoreDraft(selectedServerId, null);
    run.clearTranscript();
  };

  const openStoredSession = (session: ChatSession, messages: ChatMessage[]): void => {
    if (liveView.current.running) return;
    const serverId = useWorkspaceStore.getState().selectedServerId;
    if ((session.serverId ?? null) !== (livePreviewChat ? null : serverId)) {
      setContextError("这段对话属于其他服务器，请先切换目标。");
      return;
    }
    rememberCurrentDraft();
    sessions.openStoredSession(session);
    run.loadTranscript(entriesFromMessages(messages));
    setRunContext(session.serverId ?? "全局工作区");
    restoreDraft(serverId, session.id);
    setHistoryOpen(false);
  };

  const openRecentSession = async (sessionId: string): Promise<void> => {
    if (run.running || openingSession || resettingContext) return;
    const scope = selectedServerId;
    setOpeningSession(true);
    setContextError(null);
    try {
      const detail = await callDesktop(IPC_COMMANDS.chatSessionGet, { sessionId });
      if (useWorkspaceStore.getState().selectedServerId === scope) openStoredSession(detail.session, detail.messages);
    } catch (cause) {
      if (useWorkspaceStore.getState().selectedServerId === scope) setContextError(`打开对话失败：${errorMessage(cause)}`);
    } finally {
      setOpeningSession(false);
    }
  };

  const resetContext = async (): Promise<void> => {
    if (run.running || resettingContext || !sessions.activeSessionId) return;
    const resetSession = sessions.activeSessionId;
    const resetScope = selectedServerId;
    setResettingContext(true);
    setContextError(null);
    try {
      const result = await callDesktop(IPC_COMMANDS.agentContextReset, { sessionId: resetSession, focusServerId: resetScope ?? undefined });
      if (!result.reset) throw new Error("上下文未被重置，请重试。");
      const cached = scopeViews.current.get(resetScope);
      if (cached?.session?.id === resetSession) scopeViews.current.set(resetScope, { ...cached, entries: [] });
      if (scopeRef.current !== resetScope || liveView.current.sessionId !== resetSession) return;
      run.clearTranscript();
      setHistoryOpen(false);
    } catch (cause) {
      setContextError(`重置上下文失败：${errorMessage(cause)}`);
    } finally {
      setResettingContext(false);
    }
  };

  return (
    <aside
      id="agent-panel"
      ref={panel.panelRef}
      className={`agent-panel ${panel.panelClass}`}
      aria-label="对话"
      aria-hidden={!agentOpen}
      onAnimationEnd={panel.onAnimationEnd}
    >
      <AgentHeader
        running={run.running || openingSession || resettingContext}
        runState={run.runState}
        tokenUsage={run.tokenUsage}
        contextUsage={run.contextUsage}
        taskTitle={sessions.title}
        agentReady={agentRunning}
        agentOpen={agentOpen}
        shell={shell}
        historyOpen={historyOpen}
        historyButtonRef={historyButtonRef}
        onToggleHistory={() => setHistoryOpen(!historyOpen)}
        onToggle={panel.toggle}
        closeButtonRef={panel.closeButtonRef}
        workspace={workspace}
        workspaceControls={workspaceControls}
        context={<CoordinatorStatus tasks={teamTasks.tasks} sessionId={sessions.activeSessionId} preview={preview} />}
        showStatus={false}
      />

      {historyOpen ? <div ref={historyPopoverRef} className="agent-task-popover" role="dialog" aria-label="切换任务">
        <AgentHistoryPane
          compact
          serverId={selectedServerId}
          activeSessionId={sessions.activeSessionId}
          restoreFocusRef={historyButtonRef}
          onClose={() => setHistoryOpen(false)}
          onNewSession={() => startNewSession()}
          onOpenSession={openStoredSession}
          onSessionUpdated={sessions.noteSessionUpdated}
          onSessionDeleted={(sessionId) => {
            if (sessions.noteSessionDeleted(sessionId)) startNewSession();
          }}
          onResetContext={sessions.activeSessionId && shell ? () => void resetContext() : undefined}
          resetting={resettingContext}
        />
      </div> : null}
        <WorkspaceLayers tools={workspaceWorkpages}>
          <WorkspaceConversation enabled={Boolean(workspaceWorkpages)} empty={run.entries.length === 0}>
            <ConversationWatchStatus onOpen={onOpenTeam} />
            <AgentFeed
              entries={coordinatorEntries(run.entries)}
              running={run.running}
              runState={run.runState}
              lastUserEntry={lastUserEntry(run.entries)}
              canSend={canSend}
              onRetry={(entry) => void send(entry.text, entry.attachments ?? [], false)}
              onApproval={run.respondApproval}
              pendingApprovalIds={run.pendingApprovalIds}
              approvalStatuses={run.approvalStatuses}
              feedRef={follow.feedRef}
              onScroll={follow.onScroll}
            ><CoordinatorTasks runtime={teamTasks} sessionId={sessions.activeSessionId} /></AgentFeed>

            <div className="agent-input-region">
              <AgentComposer
                notices={<>
                  {sessions.archived ? <p className="agent-history-archived" role="status">当前对话已归档，恢复后才可以继续发送。</p> : null}
                  {!preview || livePreviewChat ? <AgentNotices
                    shell={shell}
                    agentRunning={agentRunning}
                    providerReady={models.providerReady}
                    agentExited={Boolean(agentStatus.data?.lastExit)}
                    restart={agentStatus.data?.restart}
                    statusUnreadable={agentStatus.isError}
                    spawning={spawnAgent.isPending}
                    spawnError={spawnAgent.error?.message}
                    onSpawn={() => spawnAgent.mutate()}
                    onOpenSettings={() => setPrimary("settings")}
                  /> : null}
                  {sessions.error ? <div className="agent-history-error agent-history-error-inline" role="status">{sessions.error}</div> : null}
                  {contextError ? <div className="agent-history-error agent-history-error-inline" role="alert">{contextError}</div> : null}
                  {roles.notice ? <div className="agent-history-error agent-history-error-inline" role="status">{roles.notice}</div> : null}
                </>}
                status={<>
                  {livePreviewChat ? <span>模拟服务器 · 仅对话</span> : null}
                  <button type="button" className="agent-composer-tool agent-scope-trigger" disabled={run.running || !selectedServerId} aria-label={selectedServerId ? `当前范围：${focusServer?.name ?? selectedServerId}，切换到全局对话` : "当前范围：全局工作区"} title={run.running ? runContext ?? "运行中" : selectedServerId ? `${focusServer?.name ?? selectedServerId} · 切换到全局` : "全局工作区"} onClick={() => useWorkspaceStore.getState().selectServer(null)}><Icon name={selectedServerId ? "servers" : "projects"} size="md" /></button>
                  <AgentRunStatus compact preview={preview && !livePreviewChat} running={run.running} runState={run.runState} tokenUsage={run.tokenUsage} contextUsage={run.contextUsage} agentReady={agentRunning} shell={shell} />
                </>}
                prompt={prompt}
                onPromptChange={setPrompt}
                attachments={attachments}
                onAttachmentsChange={setAttachments}
                onSubmit={handleSubmit}
                onStop={() => void run.stop()}
                running={run.running}
                stopping={run.stopping}
                canSend={canSend}
                sendDisabledReason={preview && !livePreviewChat ? "请在桌面窗口发送" : !agentRunning ? "请先启动运行服务" : !models.providerReady ? "请先配置模型" : !run.listening ? "正在连接对话" : "当前对话不可发送"}
                canStop={Boolean(run.runId) && !run.stopping}
                permissionMode={permissionMode}
                onPermissionModeChange={(mode) => setPreferences({ agentPermissionMode: mode })}
                runMode={runMode}
                onRunModeChange={(mode) => setPreferences({ agentRunMode: mode })}
                delivery={delivery}
                onDeliveryChange={(value) => setPreferences({ agentDelivery: value })}
                runPolicy={runPolicy}
                onRunPolicyChange={setRunPolicy}
                targetEnvironment={targetEnvironment}
                models={models.modelChoices}
                selectedModelKey={models.selectedModelKey}
                selectedModelLabel={models.selectedModelLabel}
                onSelectModel={models.selectModelKey}
                reasoningLevels={modelReasoningLevels}
                reasoningEffort={reasoningEffort}
                onReasoningEffortChange={(value) => setPreferences({ agentReasoningEffort: value })}
                mentions={composerMentions}
                roles={roles.roles}
                selectedRoleId={selectedRoleId}
                onSelectRole={setSelectedRoleId}
                commandContext={commandContext}
              />
              {run.entries.length === 0 ? <AgentRecentConversations
                sessions={recent.data?.sessions ?? []}
                busy={run.running || openingSession || resettingContext}
                error={recent.isError}
                onOpen={(id) => void openRecentSession(id)}
                onShowAll={() => setHistoryOpen(true)}
                onRetry={() => void recent.refetch()}
              /> : null}
            </div>
          </WorkspaceConversation>
        </WorkspaceLayers>
    </aside>
  );
}

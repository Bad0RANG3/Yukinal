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

import type { ChatMessage, ChatSession } from "@yukinal/shared";
import { useCallback, useEffect, useMemo, useState } from "react";

import { Icon } from "../../components/Icon.js";
import { isDesktopShell } from "../../lib/ipc.js";
import { useAgentStatus, useSpawnAgent } from "../../lib/runtime.js";
import { useServers } from "../../lib/servers.js";
import { usePreferencesStore } from "../../stores/preferences-store.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { AgentComposer } from "./AgentComposer.js";
import { AgentFeed } from "./AgentFeed.js";
import { AgentHeader } from "./AgentHeader.js";
import { AgentHistoryPane } from "./AgentHistoryPane.js";
import { AgentNotices } from "./AgentNotices.js";
import {
  commandHelpText,
  resolveMentionedServer,
  unavailableReason,
  type CommandContext,
  type MentionCandidate,
  type Submission,
} from "./composer-triggers.js";
import { entriesFromMessages, lastUserPrompt } from "./transcript.js";
import { requestPolicyId, type RunPolicyChoice } from "./run-policy.js";
import { useAgentModels } from "./useAgentModels.js";
import { useAgentPanelShell } from "./useAgentPanelShell.js";
import { useAgentRun } from "./useAgentRun.js";
import { useChatSessions } from "./useChatSessions.js";
import { useFeedFollow } from "./useFeedFollow.js";

export function AgentPanel({ onCloseStart, onCloseEnd }: { onCloseStart?: () => void; onCloseEnd?: () => void }) {
  const agentStatus = useAgentStatus();
  const spawnAgent = useSpawnAgent();
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const agentOpen = useWorkspaceStore((state) => state.agentOpen);
  const setAgentOpen = useWorkspaceStore((state) => state.setAgentOpen);
  const toggleAgent = useWorkspaceStore((state) => state.toggleAgent);
  const setPrimary = useWorkspaceStore((state) => state.setPrimary);
  const permissionMode = usePreferencesStore((state) => state.agentPermissionMode);
  const runMode = usePreferencesStore((state) => state.agentRunMode);
  const setPreferences = usePreferencesStore((state) => state.setPreferences);

  const shell = isDesktopShell();
  const agentRunning = agentStatus.data?.running === true;
  const servers = useServers();
  const focusServer = servers.data?.find((server) => server.id === selectedServerId);

  const models = useAgentModels();
  const sessions = useChatSessions();
  // 「收到审批时把面板带到前景」是导航决策，所以由面板注入，而不是让运行模块去读 store。
  const run = useAgentRun({
    sidecarRunning: agentStatus.data?.running,
    onAssistantMessage: (text) => void sessions.recordAssistantMessage(text),
    onApprovalRequested: () => setAgentOpen(true),
  });

  // 草稿和「正在跑」放在 workspace store 里，是因为首次使用引导要先替用户填好
  // 输入框、并避免覆盖正在进行的任务 —— 两者都是跨面板的事实，不只是本组件状态。
  const prompt = useWorkspaceStore((state) => state.agentDraft);
  const setPrompt = useWorkspaceStore((state) => state.setAgentDraft);
  const setAgentBusy = useWorkspaceStore((state) => state.setAgentBusy);
  useEffect(() => {
    setAgentBusy(run.running);
    return () => setAgentBusy(false);
  }, [run.running, setAgentBusy]);
  const [runContext, setRunContext] = useState<string | null>(null);
  const [historyOpen, setHistoryOpen] = useState(false);
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
  });

  const canSend = shell && agentRunning && run.listening && models.providerReady && !sessions.archived;

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

  const send = async (raw: string): Promise<void> => {
    const text = raw.trim();
    if (!text || !canSend || run.running) return;
    // 提示词里的 @服务器 决定这次运行的目标；没提及时才回落到当前选中的服务器。
    const mentioned = resolveMentionedServer(text, mentionCandidates);
    const focusServerId = mentioned?.id ?? selectedServerId;
    setRunContext(mentioned?.label ?? focusServer?.name ?? "全局工作区");
    follow.pinToBottom();
    setPrompt("");
    const outcome = await run.start({
      prompt: text,
      persistUserMessage: (messageId) => sessions.recordUserMessage(text, messageId, focusServerId),
      providerId: models.selectedProviderId,
      model: models.selectedModel,
      focusServerId,
      permissionMode,
      mode: runMode,
      // 「按环境自动」在这里就是「不带这个字段」，由 requestPolicyId 保证。
      policyId: requestPolicyId(runPolicy),
    });
    // 没跑起来就把输入还给用户，别让他重新打一遍。
    if (outcome !== "started") setPrompt(text);
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
        startNewSession();
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

  const startNewSession = (): void => {
    if (run.running) return;
    sessions.startNewSession();
    setHistoryOpen(false);
    setRunContext(null);
    setPrompt("");
    run.clearTranscript();
  };

  const openStoredSession = (session: ChatSession, messages: ChatMessage[]): void => {
    if (run.running) return;
    sessions.openStoredSession(session);
    run.loadTranscript(entriesFromMessages(messages));
    setRunContext(session.serverId ?? "全局工作区");
    setPrompt("");
    setHistoryOpen(false);
  };

  return (
    <aside
      id="agent-panel"
      ref={panel.panelRef}
      className={`agent-panel ${panel.panelClass}`}
      aria-hidden={!agentOpen}
      onAnimationEnd={panel.onAnimationEnd}
    >
      <AgentHeader
        running={run.running}
        runState={run.runState}
        agentReady={agentRunning}
        agentOpen={agentOpen}
        shell={shell}
        historyOpen={historyOpen}
        onToggleHistory={() => setHistoryOpen((current) => !current)}
        onToggle={panel.toggle}
        closeButtonRef={panel.closeButtonRef}
      />

      {historyOpen ? (
        <AgentHistoryPane
          activeSessionId={sessions.activeSessionId}
          onClose={() => setHistoryOpen(false)}
          onNewSession={startNewSession}
          onOpenSession={openStoredSession}
          onSessionUpdated={sessions.noteSessionUpdated}
          onSessionDeleted={(sessionId) => {
            if (sessions.noteSessionDeleted(sessionId)) startNewSession();
          }}
        />
      ) : (
        <>
          <div className="agent-context">
            <Icon name="servers" size="sm" />
            <span title={runContext ?? focusServer?.name ?? "全局工作区"}>
              {run.running ? runContext : focusServer?.name ?? "全局工作区"}
            </span>
            <small>{run.running ? "本次运行目标" : "当前上下文"}</small>
          </div>
          {sessions.archived ? <p className="agent-history-archived" role="status">当前对话已归档，恢复后才可以继续发送。</p> : null}

          <AgentFeed
            entries={run.entries}
            running={run.running}
            runState={run.runState}
            lastUserPrompt={lastUserPrompt(run.entries)}
            canSend={canSend}
            onRetry={(text) => void send(text)}            onApproval={run.respondApproval}
            pendingApprovalIds={run.pendingApprovalIds}
            approvalStatuses={run.approvalStatuses}
            feedRef={follow.feedRef}
            onScroll={follow.onScroll}
          >
            <AgentNotices
              shell={shell}
              agentRunning={agentRunning}
              providerReady={models.providerReady}
              agentExited={Boolean(agentStatus.data?.lastExit)}
              statusUnreadable={agentStatus.isError}
              spawning={spawnAgent.isPending}
              spawnError={spawnAgent.error?.message}
              onSpawn={() => spawnAgent.mutate()}
              onOpenSettings={() => setPrimary("settings")}
            />
          </AgentFeed>

          {sessions.error ? <div className="agent-history-error agent-history-error-inline" role="status">{sessions.error}</div> : null}

          <AgentComposer
            prompt={prompt}
            onPromptChange={setPrompt}
            onSubmit={handleSubmit}
            onStop={() => void run.stop()}
            running={run.running}
            stopping={run.stopping}
            canSend={canSend}
            canStop={Boolean(run.runId) && !run.stopping}
            permissionMode={permissionMode}
            onPermissionModeChange={(mode) => setPreferences({ agentPermissionMode: mode })}
            runMode={runMode}
            onRunModeChange={(mode) => setPreferences({ agentRunMode: mode })}
            runPolicy={runPolicy}
            onRunPolicyChange={setRunPolicy}
            models={models.modelChoices}
            selectedModelKey={models.selectedModelKey}
            selectedModelLabel={models.selectedModelLabel}
            onSelectModel={models.selectModelKey}
            mentions={mentionCandidates}
            commandContext={commandContext}
          />
        </>
      )}
    </aside>
  );
}

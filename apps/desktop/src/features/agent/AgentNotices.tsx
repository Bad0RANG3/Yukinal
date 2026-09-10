/**
 * 「为什么现在不能提问」的唯一出口。
 *
 * 三种阻塞理由互斥且有序：不在桌面壳里 → sidecar 没在跑 → 没配 provider。
 * 按这个顺序解释，用户永远先看到那个真正卡住自己的原因。
 */

export function AgentNotices({
  shell,
  agentRunning,
  providerReady,
  agentExited,
  statusUnreadable,
  spawning,
  spawnError,
  onSpawn,
  onOpenSettings,
}: {
  shell: boolean;
  agentRunning: boolean;
  providerReady: boolean;
  /** 上一次 sidecar 退出记录存在，可以重新启动。 */
  agentExited: boolean;
  /** 连 Agent 状态都读不到。 */
  statusUnreadable: boolean;
  spawning: boolean;
  spawnError?: string;
  onSpawn: () => void;
  onOpenSettings: () => void;
}) {
  if (shell && agentRunning && providerReady) return null;

  return (
    <p className="agent-notice" role="status" aria-live="polite">
      {!shell ? (
        "启动 Yukinal 桌面应用后即可直接提问，无需先添加服务器。"
      ) : !agentRunning ? (
        <>
          <span>{agentExited ? "Agent 已退出，可以重新启动。" : statusUnreadable ? "无法读取 Agent 状态，可以尝试重新启动。" : "Agent 正在启动或尚未启动。"}</span>
          <button type="button" className="text-button" disabled={spawning} onClick={onSpawn}>
            {spawning ? "启动中…" : "启动 / 重试"}
          </button>
          {spawnError ? <span className="agent-notice-error">{spawnError}</span> : null}
        </>
      ) : (
        <>
          配置 AI Provider 后即可直接提问；不需要先添加服务器。
          <button type="button" className="text-button" onClick={onOpenSettings}>前往设置</button>
        </>
      )}
    </p>
  );
}

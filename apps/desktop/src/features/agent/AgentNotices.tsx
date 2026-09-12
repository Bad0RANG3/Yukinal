/**
 * 「为什么现在不能提问」的唯一出口。
 *
 * 三种阻塞理由互斥且有序：不在桌面壳里 → sidecar 没在跑 → 没配 provider。
 * 按这个顺序解释，用户永远先看到那个真正卡住自己的原因。
 *
 * 自动恢复（ADR 0010）带来两个不再属于「被阻塞」的状态，所以它们各自先说：
 * 崩溃后已经被自动重启（什么都不用做，只是解释刚刚发生了什么），以及自动恢复的
 * 预算已经用完（这时才需要用户动手，而且必须说清楚不会再有下一次自动尝试）。
 */

import type { RestartRecord } from "@yukinal/shared";

export function AgentNotices({
  shell,
  agentRunning,
  providerReady,
  agentExited,
  restart,
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
  /** 自动恢复记录；缺省表示既没有待进行的重启，也没有用完预算。 */
  restart?: RestartRecord | undefined;
  /** 连 Agent 状态都读不到。 */
  statusUnreadable: boolean;
  spawning: boolean;
  spawnError?: string;
  onSpawn: () => void;
  onOpenSettings: () => void;
}) {
  // 预算用尽优先于一切：此时 sidecar 没在跑，而下一条分支会说「可以重新启动」，
  // 却不解释为什么它没有自己起来。用户需要知道自动恢复已经停了，否则会一直等。
  if (shell && restart?.exhausted) {
    return (
      <p className="agent-notice" role="status" aria-live="polite">
        <span>
          {`Agent 崩溃后已自动重启 ${restart.attempt}/${restart.maxAttempts} 次仍未成功，不会再次自动尝试。`}
        </span>
        <button type="button" className="text-button" disabled={spawning} onClick={onSpawn}>
          {spawning ? "启动中…" : "重新启动"}
        </button>
        {spawnError ? <span className="agent-notice-error">{spawnError}</span> : null}
      </p>
    );
  }

  if (shell && agentRunning && providerReady) {
    // 已经在跑了，但刚刚是崩溃后回来的：这不是阻塞，是一次解释——否则用户只会看到
    // 上一条回答凭空中断，而界面上没有任何痕迹。
    if (!restart) return null;
    return (
      <p className="agent-notice" role="status" aria-live="polite">
        {`Agent 刚刚崩溃过，已自动重启（第 ${restart.attempt}/${restart.maxAttempts} 次）。上一次运行已中断。`}
      </p>
    );
  }

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

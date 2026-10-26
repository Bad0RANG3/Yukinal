import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";
import { Icon } from "../../components/Icon.js";
import { SecondaryWindow, PropertyList, PropertySection } from "../../components/SecondaryWindow.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { errorMessage, formatTimestamp } from "../../lib/format.js";
import { useWorkpagesStore } from "../workpages/workpages-store.js";
import { artifactFromContent } from "../workpages/workpage-model.js";
import { TEAM_HEARTBEAT_MS } from "./team-operations.js";
import { useSubagentStore, SUBAGENT_TASK_LABEL } from "../agent/subagent-state.js";
import { teamBoardCards, type TeamBoardCard } from "./team-board.js";

const TABS = [{ id: "overview", label: "概览" }, { id: "results", label: "证据与产物" }, { id: "execution", label: "执行信息" }] as const;
export function TaskPropertiesWindow({ card, serverName, onClose }: { card: TeamBoardCard; serverName: string; onClose: () => void }) {
  const [tab, setTab] = useState("overview");
  const background = useSubagentStore((state) => state.tasks.find((task) => task.runId === card.run?.id));
  const usage = background?.usage ?? card.run?.usage;
  const detail = useQuery({ queryKey: ["investigationTask", card.task?.id], enabled: Boolean(card.task) && isDesktopShell(), staleTime: TEAM_HEARTBEAT_MS,
    queryFn: () => callDesktop(IPC_COMMANDS.investigationTaskGet, { taskId: card.task!.id }),
  });
  const task = detail.data?.task ?? card.task;
  const status = task ? teamBoardCards([task], card.run ? [card.run] : [])[0]?.status ?? card.status : background ? SUBAGENT_TASK_LABEL[background.state] : card.status;
  const updatedAt = task?.updatedAt ?? (card.updatedAt > 0 ? new Date(card.updatedAt).toISOString() : undefined);
  return <SecondaryWindow title="任务属性" subtitle={card.goal || "附件任务"} icon="board" tabs={TABS} activeTab={tab} onTabChange={setTab} onClose={onClose}
    footer={<><span>{card.preview ? "模拟反馈" : background ? "后台 subagent 任务 · 已保存" : task ? "已保存任务" : "本窗口对话运行"}</span>{updatedAt ? <time dateTime={updatedAt}>更新于 {formatTimestamp(updatedAt)}</time> : null}</>}>
    {detail.isError ? <p className="agent-nist-notice is-error" role="alert">属性读取失败：{errorMessage(detail.error)}<button type="button" className="text-button" onClick={() => void detail.refetch()}>重试</button></p> : null}
    {tab === "overview" ? <>
      <PropertySection title="任务目标"><p>{task?.objective || card.goal || "附件任务"}</p></PropertySection>
      <PropertySection title="属性"><PropertyList items={[
        { label: "状态", value: status }, { label: "执行者", value: background ? "Subagent" : "运维 Agent" }, { label: "服务器", value: serverName },
        { label: "来源", value: card.preview ? "仅界面演示，不连接服务器" : background ? "运维 Agent 拆分的独立子任务" : task ? "已保存任务" : "当前窗口会话" },
      ]} /></PropertySection>
      {task?.successCriteria.length ? <PropertySection title="完成条件"><ul>{task.successCriteria.map((criterion, index) => <li key={index}>{criterion}</li>)}</ul></PropertySection> : null}
      {task?.lastFailure ? <PropertySection title="最近失败"><p className="property-failure">{task.lastFailure.message}</p></PropertySection> : null}
    </> : null}
    {tab === "results" ? <>
      {background ? <PropertySection title={background.error ? "执行问题" : "Subagent 结果"}><p style={{ whiteSpace: "pre-wrap" }}>{background.result || background.error || "任务尚未返回结果。"}</p></PropertySection> : null}
      {detail.isLoading ? <p role="status">读取证据与产物…</p> : null}
      <PropertySection title={`产物${detail.data ? ` · ${detail.data.artifacts.length}` : ""}`}>
        {detail.data?.artifacts.length ? detail.data.artifacts.map((artifact) => <button key={artifact.id} type="button" className="team-board-artifact" onClick={() => { useWorkpagesStore.getState().openArtifact(artifactFromContent(artifact.id, artifact.title, artifact.content, card.serverId)); onClose(); }}><Icon name="file" size="sm" /><span>{artifact.title}</span><Icon name="externalLink" size="xs" /></button>) : <p className="property-empty">{detail.isError ? "读取失败，尚未同步" : detail.isLoading ? "读取中…" : "暂无产物"}</p>}
      </PropertySection>
      <PropertySection title={`证据${detail.data ? ` · ${detail.data.evidence.length}` : ""}`}>
        {detail.data?.evidence.length ? detail.data.evidence.map((evidence) => <div className="property-evidence" key={evidence.id}><strong>{evidence.sourceTool} · {formatTimestamp(evidence.collectedAt)}</strong><p>{evidence.inputSummary || "已记录工具结果"}</p></div>) : <p className="property-empty">{detail.isError ? "读取失败，尚未同步" : detail.isLoading ? "读取中…" : "暂无证据"}</p>}
      </PropertySection>
    </> : null}
    {tab === "execution" ? <>
      <PropertySection title="执行归属"><PropertyList items={[
        { label: "任务 ID", value: task?.id ?? background?.id ?? "未创建持久任务", mono: true },
        { label: "会话 ID", value: card.run?.sessionId ?? "未记录", mono: true },
        { label: "运行 ID", value: card.run?.id ?? task?.activeRunId ?? "未记录", mono: true },
        { label: "服务器 ID", value: card.serverId ?? "全局工作区", mono: true },
      ]} /></PropertySection>
      <PropertySection title="用量"><p>{card.preview ? "界面演示不产生模型用量" : usage && usage.reportedRequests !== 0 ? `${usage.unreportedRequests ? "至少 " : ""}${(usage.inputTokens + usage.outputTokens).toLocaleString()} token 已上报${usage.unreportedRequests ? ` · ${usage.unreportedRequests} 次请求未上报` : ""}` : "本次用量尚未上报"}</p></PropertySection>
      {task ? <PropertySection title="执行预算"><PropertyList items={[{ label: "步骤上限", value: task.budget.maxSteps }, { label: "时长上限", value: `${Math.round(task.budget.maxRunMs / 1000)} 秒` }, { label: "尝试上限", value: task.budget.maxAttempts }]} /></PropertySection> : null}
    </> : null}
  </SecondaryWindow>;
}

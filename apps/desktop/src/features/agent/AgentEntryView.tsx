/**
 * 动态里每一行的渲染。
 *
 * 纯展示：只接一个 Entry 和它在审批上的两个状态位，不读 store、不发 IPC。
 * 四类行各自长什么样，全部集中在这里，因此调整样式不必碰运行逻辑。
 *
 * Agent 的回复是唯一走结构化渲染的一类（`components/MarkdownText.js`），因为它是
 * 模型写回来的、带格式的正文；你说的话与应用自己的提示保持纯文本 —— 那两处出现
 * `#` 或 `-` 时，它是你要说的话，不是要渲染的标记。
 */

import type {
  AgentDocumentPromptPart,
  AgentImagePromptPart,
  AgentTextFilePromptPart,
  ApprovalDecision,
  ApprovalRequest,
} from "@yukinal/shared";
import { memo } from "react";

import { CopyButton } from "../../components/CopyButton.js";
import { Icon } from "../../components/Icon.js";
import { KeywordText } from "../../components/KeywordText.js";
import { MarkdownText } from "../../components/MarkdownText.js";
import { conversationReplyCopyText } from "../../lib/conversation-reply-copy.js";
import { approvalSourceLabel, decisionLabel, resultLabel, riskLabel } from "../../lib/labels.js";
import { imageAttachmentUrl } from "./image-attachments.js";
import { ApprovalCard } from "./ApprovalCard.js";
import { agentFailureLabel, type Entry } from "./transcript.js";

export const AgentEntryView = memo(function AgentEntryView({
  entry,
  turnAnchor,
  onApproval,
  approvalBusy,
  approvalStatus,
  streaming = false,
  intermediate = false,
  isNewEntry = false,
}: {
  entry: Entry;
  turnAnchor?: string;
  onApproval: (approval: ApprovalRequest, decision: ApprovalDecision) => Promise<void>;
  approvalBusy: boolean;
  approvalStatus?: string;
  /** True while this row is the one the model is still writing into. */
  streaming?: boolean;
  /** Public progress within a still unfinished turn, separate from private reasoning. */
  intermediate?: boolean;
  /** A newly appended live row; history loading and approvals never animate. */
  isNewEntry?: boolean;
}) {
  const newEntryClass = isNewEntry ? " is-new-entry" : "";
  switch (entry.kind) {
    /* 你与 Agent 的正文是动态里**唯一**能拖动选中的东西（见 styles.css 的
       `.agent-entry-body`）：标签、工具卡片、审批按钮都不参与选择，一次拖动
       得到的就是要说的话本身，而不是「你 / Agent」这些框。 */
    case "user":
      return (
        <div id={turnAnchor} data-conversation-turn={turnAnchor ? "" : undefined} className={`agent-entry agent-entry-user${newEntryClass}`}>
          <span className="entry-label">你</span>
          <div className="agent-entry-body">
            {entry.attachments?.some((attachment) => attachment.type === "image") ? (
              <ul className="agent-user-attachments" aria-label="消息图片">
                {entry.attachments
                  ?.filter(
                    (attachment): attachment is AgentImagePromptPart =>
                      attachment.type === "image",
                  )
                  .map((image, index) => (
                  <li key={`${image.name ?? "image"}-${index}`}>
                    <img
                      src={imageAttachmentUrl(image)}
                      alt={image.name ?? `图片 ${index + 1}`}
                      loading="lazy"
                      referrerPolicy="no-referrer"
                    />
                  </li>
                ))}
              </ul>
            ) : null}
            {entry.attachments?.some((attachment) => attachment.type === "file") ? (
              <ul className="agent-user-file-attachments" aria-label="消息文本文件">
                {entry.attachments
                  ?.filter(
                    (attachment): attachment is AgentTextFilePromptPart =>
                      attachment.type === "file",
                  )
                  .map((file, index) => (
                    <li
                      key={`${file.name}-${index}`}
                      className="agent-user-file-attachment"
                    >
                      <Icon name="file" size="sm" />
                      <span>{file.name}</span>
                    </li>
                  ))}
              </ul>
            ) : null}
            {entry.attachments?.some((attachment) => attachment.type === "document") ? (
              <ul className="agent-user-file-attachments" aria-label="消息 PDF">
                {entry.attachments
                  ?.filter(
                    (attachment): attachment is AgentDocumentPromptPart =>
                      attachment.type === "document",
                  )
                  .map((document, index) => (
                    <li
                      key={`${document.name}-${index}`}
                      className="agent-user-file-attachment"
                    >
                      <Icon name="file" size="sm" />
                      <span>{document.name}</span>
                    </li>
                  ))}
              </ul>
            ) : null}
            {entry.text ? <span>{entry.text}</span> : null}
          </div>
        </div>
      );
    case "assistant":
      return (
        <div className={`agent-entry agent-entry-assistant${intermediate ? " agent-entry-progress" : ""}${streaming ? " agent-entry-streaming" : ""}${newEntryClass}`} data-reply-phase={intermediate ? "progress" : undefined}>
          <span className="entry-label">{intermediate ? "进展" : "回复"}</span>
          <div className="agent-reply-meta">
            <strong>{entry.participant?.label ?? "Agent"}</strong>
            {intermediate ? <span className="agent-progress-label">进展</span> : null}
            {entry.participant?.kind === "subagent" ? <span>子 Agent</span> : null}
            {entry.model ? <span title={entry.model}>{entry.model}</span> : null}
          </div>
          <MarkdownText text={entry.text || "…"} className="agent-entry-body agent-markdown" streaming={streaming} />
          {streaming ? <span className="agent-stream-status" role="status"><span className="agent-stream-caret" aria-hidden="true" />正在输出</span> : null}
          {entry.text ? <div className="agent-reply-actions">
            <CopyButton text={conversationReplyCopyText(entry.text)} label={streaming ? "复制当前内容" : intermediate ? "复制进展" : "复制回答"} ariaLabel={streaming ? "复制当前已生成内容" : intermediate ? "复制完整进展" : "复制完整回答"} className="agent-entry-copy" />
          </div> : null}
        </div>
      );
    case "reasoning":
      return (
        <details className={`agent-reasoning${newEntryClass}`} aria-busy={streaming || undefined}>
          <summary className="agent-reasoning-summary">
            <span className="agent-reasoning-indicator" aria-hidden="true" />
            <span className="agent-reasoning-caption">{streaming ? "正在思考" : "思考过程"}</span>
            <span className="agent-reasoning-count">{Array.from(entry.text).length.toLocaleString()} 字符</span>
          </summary>
          <pre className="agent-reasoning-text">{entry.text}</pre>
        </details>
      );
    case "tool":
      return <ToolCard entry={entry} isNewEntry={isNewEntry} />;
    /* 应用自己的话，不是模型输出 —— 因此用中性样式并标注来源，
       避免读者把 /help 的回复当成 Agent 说过的话。 */
    case "system":
      return (
        <div className={`agent-entry agent-entry-system${newEntryClass}`}>
          <span className="entry-label">本机</span>
          <pre className="agent-entry-system-text">{entry.text}</pre>
        </div>
      );
    case "error":
      return <div className={`agent-error${newEntryClass}`} title={entry.text}>{agentFailureLabel(entry.text)}</div>;
    case "approval":
      return <ApprovalCard approval={entry.approval} onApproval={onApproval} busy={approvalBusy} status={approvalStatus} />;
  }
});

/** One collapsible row keeps the actual call, status and result together. */
function ToolCard({ entry, isNewEntry = false }: { entry: Extract<Entry, { kind: "tool" }>; isNewEntry?: boolean }) {
  const { result } = entry;
  return (
    <details className={`tool-card tool-card-${result ? result.status : "running"}${isNewEntry ? " is-new-entry" : ""}`}>
      <summary className="tool-card-heading">
        <Icon name={result?.status === "failed" ? "warning" : "terminal"} size="xs" />
        <span className="tool-name" title={entry.toolName}>{entry.purpose ?? toolActionLabel(entry.toolName)}</span>
        <span className="tool-target-preview" title={entry.target}>{entry.target}</span>
        <span className={`tool-status tool-status-${result ? result.status : "running"}`} title={result?.status === "failed" ? result.error ?? result.summary : undefined}>
          {result ? resultLabel(result.status) : "执行中…"}
        </span>
      </summary>
      <div className="tool-card-disclosure">
      <code className="tool-call-name">{entry.toolName}</code>
      <div className="tool-card-meta">
        <code><KeywordText text={entry.target} /></code>
        <span className={`risk-badge risk-${entry.riskLevel}`}>风险：{riskLabel(entry.riskLevel)}</span>
        <span className={`decision-badge decision-${entry.decision}`}>{decisionLabel(entry.decision)}</span>
        {entry.approvedBy ? <span className="decision-badge decision-source">{approvalSourceLabel(entry.approvedBy)}</span> : null}
        {result ? <span className="tool-duration">{result.durationMs} ms</span> : null}
      </div>
      {result?.status === "failed" ? <p className="tool-failure-reason">{result.error || result.summary || "工具执行失败"}</p> : null}
      {result ? <ToolResultBody toolName={entry.toolName} summary={result.summary} failed={result.status === "failed"} /> : null}
      {result?.errorCode ? <code className="tool-error-code">{result.errorCode}</code> : null}
      </div>
    </details>
  );
}

function ToolResultBody({ toolName, summary, failed }: { toolName: string; summary: string; failed: boolean }) {
  let output: { stdout: string; stderr?: string; exitCode?: number; stdoutTruncated?: boolean; stderrTruncated?: boolean } | undefined;
  if (toolName === "server.exec") {
    try {
      const parsed: unknown = JSON.parse(summary);
      if (parsed && typeof parsed === "object" && "stdout" in parsed && typeof parsed.stdout === "string") {
        output = parsed as typeof output;
      }
    } catch { /* Partial summaries and unknown formats retain the original text. */ }
  }
  if (!output) return failed ? null : <code className="tool-result-copy"><KeywordText text={summary} /></code>;
  return <div className="tool-command-output">
    {typeof output.exitCode === "number" ? <p className="tool-output-label">退出状态 {output.exitCode}</p> : null}
    {output.stdout ? <pre className="tool-result-copy">{output.stdout}</pre> : null}
    {typeof output.stderr === "string" && output.stderr ? <><p className="tool-output-label">错误输出</p><pre className="tool-result-copy">{output.stderr}</pre></> : null}
    {output.stdoutTruncated === true || output.stderrTruncated === true ? <p className="tool-output-label">输出已截断</p> : null}
    <details className="tool-raw-output"><summary>原始摘要</summary><code className="tool-result-copy">{summary}</code></details>
  </div>;
}

const TOOL_ACTION_LABELS: Record<string, string> = {
  "server.exec": "执行服务器操作", "filesystem.read": "读取文件", "filesystem.write": "写入文件", "filesystem.edit": "编辑文件", "filesystem.list": "查看目录", "filesystem.backup": "备份文件",
  "package.inspect": "检查软件包", "package.install": "安装软件包", "server.info": "读取服务器信息", "server.services": "检查服务", "server.logs": "读取日志",
  "ssh.disk_usage": "读取磁盘用量", "ssh.exec": "运行终端命令", "server.processes": "检查进程",
  "server.containers": "检查容器", "server.files": "读取文件",
};
export function toolActionLabel(name: string): string { return TOOL_ACTION_LABELS[name] ?? name; }

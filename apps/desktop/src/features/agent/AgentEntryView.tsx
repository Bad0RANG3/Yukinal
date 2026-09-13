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

import type { ApprovalDecision, ApprovalRequest } from "@yukinal/shared";

import { Icon } from "../../components/Icon.js";
import { KeywordText } from "../../components/KeywordText.js";
import { MarkdownText } from "../../components/MarkdownText.js";
import { approvalSourceLabel, decisionLabel, resultLabel, riskLabel } from "../../lib/labels.js";
import { targetLabel, type Entry } from "./transcript.js";

export function AgentEntryView({
  entry,
  onApproval,
  approvalBusy,
  approvalStatus,
  streaming = false,
}: {
  entry: Entry;
  onApproval: (approval: ApprovalRequest, decision: ApprovalDecision) => Promise<void>;
  approvalBusy: boolean;
  approvalStatus?: string;
  /** True while this row is the one the model is still writing into. */
  streaming?: boolean;
}) {
  switch (entry.kind) {
    /* 你与 Agent 的正文是动态里**唯一**能拖动选中的东西（见 styles.css 的
       `.agent-entry-body`）：标签、工具卡片、审批按钮都不参与选择，一次拖动
       得到的就是要说的话本身，而不是「你 / Agent」这些框。 */
    case "user":
      return (
        <div className="agent-entry agent-entry-user">
          <span className="entry-label">你</span>
          <div className="agent-entry-body">{entry.text}</div>
        </div>
      );
    case "assistant":
      return (
        <div className={`agent-entry agent-entry-assistant${streaming ? " agent-entry-streaming" : ""}`}>
          <span className="entry-label">Agent</span>
          <MarkdownText text={entry.text || "…"} className="agent-entry-body agent-markdown" />
        </div>
      );
    case "reasoning":
      return (
        <details className="agent-reasoning">
          <summary className="agent-reasoning-summary">思考过程</summary>
          <pre className="agent-reasoning-text">{entry.text}</pre>
        </details>
      );
    case "tool":
      return <ToolCard entry={entry} />;
    /* 应用自己的话，不是模型输出 —— 因此用中性样式并标注来源，
       避免读者把 /help 的回复当成 Agent 说过的话。 */
    case "system":
      return (
        <div className="agent-entry agent-entry-system">
          <span className="entry-label">本机</span>
          <pre className="agent-entry-system-text">{entry.text}</pre>
        </div>
      );
    case "error":
      return <div className="agent-error">{entry.text}</div>;
    case "approval":
      return (
        <div className="approval-card">
          <div className="approval-heading">
            <span className="approval-icon"><Icon name="warning" size="sm" /></span>
            <div><strong>需要审批</strong><code><KeywordText text={entry.approval.toolName} /></code></div>
          </div>
          <p>{entry.approval.reason}</p>
          <p className="approval-target">{targetLabel(entry.approval.target)}</p>
          {approvalStatus ? (
            <p className="approval-resolved" role="status">{approvalStatus}</p>
          ) : (
            <div className="approval-actions">
              <button type="button" className="approval-button approval-button-reject" disabled={approvalBusy} onClick={() => void onApproval(entry.approval, "reject")}>
                拒绝
              </button>
              <button type="button" className="approval-button approval-button-approve" disabled={approvalBusy} onClick={() => void onApproval(entry.approval, "approve_once")}>
                批准一次
              </button>
              <button type="button" className="approval-button approval-button-approve" disabled={approvalBusy} onClick={() => void onApproval(entry.approval, "approve_session")}>
                本次运行批准
              </button>
            </div>
          )}
        </div>
      );
  }
}

/**
 * 一次工具调用 —— 调用与结果合成一张卡片。
 *
 * 默认展开而不是默认折叠：这个面板的存在意义就是可追溯，把结果藏进
 * 一次点击后面等于让审计变难。`open` 只在首次挂载时写入，之后用户手动
 * 折叠不会被后续事件重渲染强行掰开（React 仅在属性值变化时才改 DOM）。
 */
function ToolCard({ entry }: { entry: Extract<Entry, { kind: "tool" }> }) {
  const { result } = entry;
  return (
    <details className={`tool-card tool-card-${result ? result.status : "running"}`} open>
      <summary className="tool-card-heading">
        <span className="tool-card-label">工具</span>
        <span className="tool-name"><KeywordText text={entry.toolName} /></span>
        <span className={`tool-status tool-status-${result ? result.status : "running"}`}>
          {result ? `${resultLabel(result.status)} · ${result.durationMs}ms` : "执行中…"}
        </span>
      </summary>
      <div className="tool-card-meta">
        <code><KeywordText text={entry.target} /></code>
        <span className={`risk-badge risk-${entry.riskLevel}`}>风险：{riskLabel(entry.riskLevel)}</span>
        <span className={`decision-badge decision-${entry.decision}`}>{decisionLabel(entry.decision)}</span>
        {entry.approvedBy ? <span className="decision-badge decision-source">{approvalSourceLabel(entry.approvedBy)}</span> : null}
      </div>
      {result ? <code className="tool-result-copy"><KeywordText text={result.summary} /></code> : null}
    </details>
  );
}

import type { ApprovalDecision, ApprovalRequest } from "@yukinal/shared";

import { CopyButton } from "../../components/CopyButton.js";
import { printableContent } from "../workpages/workpage-model.js";
import { Icon } from "../../components/Icon.js";
import { targetLabel } from "./transcript.js";

/** Presentation only: the caller retains the real ticket, submission lock and host response. */
export function ApprovalCard({ approval, onApproval, busy, status }: {
  approval: ApprovalRequest;
  onApproval: (approval: ApprovalRequest, decision: ApprovalDecision) => Promise<void>;
  busy: boolean;
  status?: string;
}) {
  const input = approval.input && typeof approval.input === "object" && !Array.isArray(approval.input)
    ? approval.input as Record<string, unknown> : null;
  const command = typeof input?.command === "string" ? input.command : null;
  const purpose = typeof input?.purpose === "string" && input.purpose.trim() ? input.purpose : approval.toolName;
  const genericReason = approval.reason === `${approval.toolName} needs your approval before it can run.`;
  const facts = approval.factsSummary.map((fact) => approvalFactLabel(fact, Boolean(status)));
  const lineCount = command === null ? 0 : command.split("\n").length;
  return <div className={`approval-card${status ? " is-resolved" : " is-pending"}`} aria-busy={busy || undefined}>
    <div className="approval-heading">
      <span className="approval-icon"><Icon name="shield" size="sm" /></span>
      <div className="approval-title"><span className="approval-caption">操作确认</span><strong>{purpose}</strong></div>
      {status ? <span className="approval-resolved" role="status">{status}</span> : <span className="approval-state">待审批</span>}
    </div>
    <div className="approval-target"><span>{targetLabel(approval.target)}</span><code>{approval.toolName}</code></div>
    {!genericReason && approval.reason ? <p className="approval-reason">{approval.reason}</p> : null}
    {facts.length ? <ul className="approval-facts" aria-label="审批依据">
      {facts.map((fact, index) => <li key={index}>{fact}</li>)}
    </ul> : null}
    <details key={`${approval.approvalId}:${Boolean(status)}`} className="approval-input" open={!status}>
      <summary><span>{command === null ? "操作参数" : "执行命令"}</span>{lineCount > 1 ? <span className="approval-line-count">{lineCount} 行</span> : null}</summary>
      {command === null
        ? <pre className="approval-input-json">{printableContent(approval.input)}</pre>
        : <>
          <pre className="approval-input-json approval-command"><code>{command}</code></pre>
          <div className="approval-command-tools"><CopyButton text={command} label="复制命令" /></div>
          <details className="approval-parameters"><summary>全部参数</summary><pre className="approval-input-json">{printableContent(approval.input)}</pre></details>
        </>}
    </details>
    {approval.callId || approval.inputFingerprint ? <details className="approval-reference"><summary>审批标识</summary>
      {approval.callId ? <p>调用 <code>{approval.callId}</code></p> : null}
      {approval.inputFingerprint ? <p>指纹 <code>{approval.inputFingerprint}</code></p> : null}
    </details> : null}
    {!status ? <div className="approval-footer">
      <p className="approval-note" role={busy ? "status" : undefined}>{busy ? "正在提交…" : approval.sessionGrantable === false ? "此操作每次都需要单独批准" : "批准范围仅限本次运行"}</p>
      <div className="approval-actions">
        <button type="button" className="approval-button approval-button-reject" disabled={busy} onClick={() => void onApproval(approval, "reject")}>拒绝</button>
        <button type="button" className="approval-button approval-button-approve" disabled={busy} onClick={() => void onApproval(approval, "approve_once")}>批准一次</button>
        {approval.sessionGrantable !== false ? <button type="button" className="approval-button approval-button-session" disabled={busy} onClick={() => void onApproval(approval, "approve_session")}>本次运行批准</button> : null}
      </div>
    </div> : null}
  </div>;
}

/** Translate known protocol facts only; unfamiliar policy explanations stay intact. */
function approvalFactLabel(fact: string, settled: boolean): string {
  const known: Record<string, string> = {
    "The tool has not run yet.": settled ? "审批发起时尚未执行" : "尚未执行",
    "Risk level: read": "风险：只读",
    "Risk level: low": "风险：低",
    "Risk level: medium": "风险：中",
    "Risk level: high": "风险：高",
    "Risk level: critical": "风险：严重",
    "Risk level: destructive": "风险：破坏性操作",
  };
  return known[fact] ?? fact;
}

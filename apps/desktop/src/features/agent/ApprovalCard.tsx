import type { ApprovalDecision, ApprovalRequest } from "@yukinal/shared";

import { Icon } from "../../components/Icon.js";
import { KeywordText } from "../../components/KeywordText.js";
import { targetLabel } from "./transcript.js";

/** Presentation only: the caller retains the real ticket, submission lock and host response. */
export function ApprovalCard({ approval, onApproval, busy, status }: {
  approval: ApprovalRequest;
  onApproval: (approval: ApprovalRequest, decision: ApprovalDecision) => Promise<void>;
  busy: boolean;
  status?: string;
}) {
  return <div className="approval-card" aria-busy={busy || undefined}>
    <div className="approval-heading">
      <span className="approval-icon"><Icon name="warning" size="sm" /></span>
      <div><strong>需要审批</strong><code><KeywordText text={approval.toolName} /></code></div>
    </div>
    <p className="approval-reason">{approval.reason}</p>
    <p className="approval-target">{targetLabel(approval.target)}</p>
    {approval.factsSummary.length ? <ul className="approval-facts" aria-label="审批依据">
      {approval.factsSummary.map((fact, index) => <li key={index}>{fact}</li>)}
    </ul> : null}
    <details className="approval-input" open>
      <summary>核对本次操作</summary>
      <pre className="agent-entry-body approval-input-json">{JSON.stringify(approval.input, null, 2) ?? "未提供参数"}</pre>
    </details>
    {approval.sessionGrantable === false ? <p className="approval-note">此操作每次都需要单独批准，不能记住到本次运行。</p> : null}
    {status ? <p className="approval-resolved" role="status">{status}</p> : <>
      {busy ? <p className="approval-pending" role="status">正在提交审批决定…</p> : null}
      <div className="approval-actions">
        <button type="button" className="approval-button approval-button-reject" disabled={busy} onClick={() => void onApproval(approval, "reject")}>拒绝</button>
        <button type="button" className="approval-button approval-button-approve" disabled={busy} onClick={() => void onApproval(approval, "approve_once")}>批准一次</button>
        {approval.sessionGrantable !== false ? <button type="button" className="approval-button approval-button-approve" disabled={busy} onClick={() => void onApproval(approval, "approve_session")}>本次运行批准</button> : null}
      </div>
    </>}
  </div>;
}

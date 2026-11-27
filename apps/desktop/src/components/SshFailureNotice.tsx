/**
 * 一次 SSH 连接失败 → 一句解释、一条下一步动作、原始报文折叠在后。
 *
 * 存在的理由：界面过去只显示 `action.error.message`，而宿主曾经把所有 SSH
 * 失败压成同一个码，于是「密码被拒」「凭据读不出来」「没确认过指纹」「超时」
 * 「代理要认证」在界面上长得一模一样，唯一的建议是「检查地址、端口、用户名及
 * 认证信息」——把五件事说成一件，用户只能挨个试。
 *
 * 现在码是真的（见 `lib/ssh-error.ts` 的说明），这个组件负责把它摆到台面上：
 *
 * 1. 先说**出了什么事**（`title`），再说**下一步做什么**（`hint`）。
 * 2. 码本身以等宽小字显示 —— 用户去搜、去贴给别人看时用得上，但不抢主体。
 * 3. 核心写的原始报文放进 `<details>`：它是唯一的一手信息，不能丢，但也不该
 *    在每次失败时占满整屏。
 *
 * 不是 SSH 失败（例如 `STORAGE_ERROR`、zod 校验失败）时退回纯文本渲染，
 * 不硬套 SSH 的建议 —— 建议错了比没有更糟。
 */

import { errorMessage } from "../lib/format.js";
import { sshFailureAdvice, stripFailureCode } from "../lib/ssh-error.js";

export function SshFailureNotice({
  error,
  className = "form-error",
}: {
  error: unknown;
  className?: string;
}) {
  const advice = sshFailureAdvice(error);
  const raw = errorMessage(error);
  if (!advice) {
    return <p className={className} role="alert">{raw}</p>;
  }
  return (
    <div className={`ssh-failure ${className}`} role="alert">
      <p className="ssh-failure-title">{advice.title}</p>
      <p className="ssh-failure-hint">{advice.hint}</p>
      <details className="ssh-failure-detail">
        <summary>
          原始报文 <code>{advice.code}</code>
        </summary>
        <pre>{stripFailureCode(raw)}</pre>
      </details>
    </div>
  );
}

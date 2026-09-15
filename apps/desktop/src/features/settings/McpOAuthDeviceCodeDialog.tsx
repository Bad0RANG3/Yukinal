/**
 * 设备码授权对话框（RFC 8628）。
 *
 * ## 它为什么是一个「显示」组件
 *
 * 授权是在 Rust 侧进行的，而且**还没结束**：`mcp_oauth_connect` 正在等用户到浏览器里确认。
 * 所以这里没有「提交」按钮 —— 用户该做的事发生在别处（浏览器、手机或另一台机器），这个窗口
 * 只负责把 `user_code`、验证链接和「还在等」说清楚，并提供唯一的本地动作：取消。
 *
 * ## 三条不会让步的规则
 *
 * 1. **代码原样显示。** `user_code` 是授权服务器给的字符串，界面不改大小写、不加空格、不
 *    做任何「友好化」——用户要把它敲进另一个设备，改一个字符就是授权失败。
 * 2. **优先用完整链接。** 服务器给了 `verification_uri_complete` 就打开它；同时把不带 code
 *    的 `verification_uri` 也显示出来，因为浏览器没预填时，用户仍然需要知道该去哪一页。
 * 3. **终态由后端的话决定。** 失败原因是 Rust 给的原文（`access_denied` / `expired_token` /
 *    超时 / 取消），这里照原样显示，不重写成一句更好听的话。
 */

import { useEffect, useRef } from "react";

import type { McpOAuthDeviceCodeEvent } from "@yukinal/shared";

import { Icon } from "../../components/Icon.js";
import { deviceCodeExpiryText, deviceCodeLink } from "../../lib/mcp.js";

/**
 * 等待、成功、失败、以及**用户自己取消**。
 *
 * 取消单列一相，是因为它不是错误：后端会把它报成一次失败的授权请求（那是对的，令牌确实
 * 没有拿到），但界面不该用一个 `role="alert"` 去吼用户刚刚主动做的事。
 */
export type McpOAuthDeviceCodePhase = "waiting" | "succeeded" | "failed" | "cancelled";

export interface McpOAuthDeviceCodeViewProps {
  prompt: McpOAuthDeviceCodeEvent;
  phase: McpOAuthDeviceCodePhase;
  /** 终态的那句话：失败时是后端原文，成功时是界面自己的确认。 */
  message?: string | null;
  /** 复制成功后的提示；`null` 表示还没复制过。 */
  copyHint?: string | null;
  onOpenLink: (url: string) => void;
  onCopyCode: () => void;
  onCancel: () => void;
  onDismiss: () => void;
  /** 由调用方传入，便于把「还剩几分钟」钉住而不是读时钟。 */
  now?: number;
}

/** 纯展示：所有输入来自 props，可以直接渲染进静态 HTML 来断言。 */
export function McpOAuthDeviceCodeView({
  prompt,
  phase,
  message = null,
  copyHint = null,
  onOpenLink,
  onCopyCode,
  onCancel,
  onDismiss,
  now,
}: McpOAuthDeviceCodeViewProps) {
  const titleId = "mcp-device-code-title";
  const expiry = deviceCodeExpiryText(prompt, now ?? Date.now());
  const waiting = phase === "waiting";

  // 根元素用 `server-form` 而不是 `settings-card`：后者自带边框与背景，而这个对话框本身
  // 就住在 `server-modal` 那张卡片里，卡中卡会多出一圈没有意义的边线（SSH 二次认证弹窗
  // 用的是同一套结构）。
  return (
    <div className="server-form mcp-device-code" role="dialog" aria-labelledby={titleId}>
      <header className="modal-header">
        <div>
          <p className="eyebrow">MCP OAuth</p>
          <h2 id={titleId}>设备码授权</h2>
        </div>
        <span className="settings-status">{waiting ? "等待授权" : "已结束"}</span>
      </header>

      <p className="form-hint">
        在浏览器里打开验证页面并输入下面的代码。<strong>Yukinal 不接触你在那一页输入的内容</strong>；
        授权服务器只把结果告诉本地这个等待中的请求。
      </p>

      <div className="mcp-device-code-value">
        <code>{prompt.userCode}</code>
        <button type="button" className="button-secondary" onClick={onCopyCode} disabled={!waiting}>
          <Icon name="copy" size="sm" /> 复制代码
        </button>
      </div>
      {copyHint ? <p className="form-hint">{copyHint}</p> : null}

      <p className="form-hint">
        验证页面：<code>{prompt.verificationUri}</code>
      </p>
      <button
        type="button"
        className="button-secondary"
        onClick={() => onOpenLink(deviceCodeLink(prompt))}
        disabled={!waiting}
      >
        <Icon name="externalLink" size="sm" /> 在浏览器里打开验证页面
      </button>
      {prompt.verificationUriComplete ? (
        <p className="form-hint">
          这个链接里已经带着代码，打开后会直接填好；上面的代码仍然有效。
        </p>
      ) : null}

      {expiry ? <p className="form-hint">{expiry}</p> : null}

      {waiting ? (
        <p className="form-hint">
          还没有收到授权结果；这个窗口会一直等到你完成，或者验证码过期。
        </p>
      ) : null}
      {phase === "succeeded" ? (
        <p className="form-success" role="status">
          {message ?? "授权已完成。"}
        </p>
      ) : null}
      {phase === "cancelled" ? (
        <p className="form-hint" role="status">
          {message ?? "已取消这次授权。"}
        </p>
      ) : null}
      {phase === "failed" ? (
        <p className="form-error" role="alert">
          {message ?? "授权没有完成。"}
        </p>
      ) : null}

      <div className="modal-actions">
        {waiting ? (
          <button type="button" className="button-secondary" onClick={onCancel}>
            取消授权
          </button>
        ) : (
          <button type="button" className="button-primary" onClick={onDismiss}>
            关闭
          </button>
        )}
      </div>
    </div>
  );
}

export interface McpOAuthDeviceCodeDialogProps extends McpOAuthDeviceCodeViewProps {
  /** `true` 时把对话框作为模态窗口打开，并聚焦到取消按钮上。 */
  modal?: boolean;
}

/**
 * 与 [`McpOAuthDeviceCodeView`] 同一份渲染，只是外面套一个真正的模态 `<dialog>`。
 *
 * 分开的理由和 `ServerAuthChallengeModal` 一样：静态渲染不能 `showModal()`，而断言的是内容，
 * 不是模态行为本身。
 */
export function McpOAuthDeviceCodeDialog({
  modal = true,
  ...view
}: McpOAuthDeviceCodeDialogProps) {
  const dialogRef = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog || !modal) return;
    if (!dialog.open) dialog.showModal();
    return () => dialog.close();
  }, [modal]);

  return (
    <dialog
      ref={dialogRef}
      className="server-modal mcp-device-code-dialog"
      aria-labelledby="mcp-device-code-title"
    >
      <McpOAuthDeviceCodeView {...view} />
    </dialog>
  );
}

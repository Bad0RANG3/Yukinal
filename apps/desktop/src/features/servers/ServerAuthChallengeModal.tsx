import { useEffect, useRef, useState, type FormEvent } from "react";

import {
  IPC_COMMANDS,
  type ServerAuthChallengeEvent,
} from "@yukinal/shared";

import { Icon } from "../../components/Icon.js";
import { callDesktop, subscribeDesktop } from "../../lib/ipc.js";

/**
 * Renders one server-issued keyboard-interactive challenge at a time.
 *
 * Responses remain component memory until submitted, then travel through a command to a
 * one-shot Rust channel. Nothing is persisted and the modal never invents a challenge.
 */
export function ServerAuthChallengeModal() {
  const [challenges, setChallenges] = useState<ServerAuthChallengeEvent[]>([]);
  const [responses, setResponses] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const dialogRef = useRef<HTMLDialogElement>(null);
  const current = challenges[0];

  useEffect(() => {
    const subscription = subscribeDesktop("server.auth_challenge", (challenge) => {
      setChallenges((current) =>
        current.some((item) => item.authId === challenge.authId)
          ? current
          : [...current, challenge],
      );
    });
    return () => subscription.stop();
  }, []);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!current) {
      dialog?.close();
      return;
    }
    setResponses(Array.from({ length: current.prompts.length }, () => ""));
    setError(null);
    if (!dialog?.open) dialog?.showModal();
    const focusFrame = requestAnimationFrame(() => {
      const input = dialog?.querySelector<HTMLInputElement>("[data-auth-input]");
      if (input) input.focus({ preventScroll: true });
      else dialog?.focus({ preventScroll: true });
    });
    const remaining = Date.parse(current.expiresAt) - Date.now();
    const expiryTimer = window.setTimeout(() => {
      setChallenges((items) => items.filter((item) => item.authId !== current.authId));
    }, Math.max(0, remaining));
    return () => {
      cancelAnimationFrame(focusFrame);
      window.clearTimeout(expiryTimer);
    };
  }, [current]);

  useEffect(() => () => dialogRef.current?.close(), []);

  const removeCurrent = (): void => {
    setChallenges((items) => items.slice(1));
  };

  const submit = async (event: FormEvent): Promise<void> => {
    event.preventDefault();
    if (!current || busy) return;
    setBusy(true);
    setError(null);
    try {
      const { accepted } = await callDesktop(IPC_COMMANDS.serverAuthRespond, {
        authId: current.authId,
        responses,
      });
      if (!accepted) throw new Error("本次认证挑战已过期或已被处理，请重新连接。");
      setResponses([]);
      removeCurrent();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };

  const cancel = async (): Promise<void> => {
    if (!current || busy) return;
    setBusy(true);
    setError(null);
    try {
      const { accepted } = await callDesktop(IPC_COMMANDS.serverAuthCancel, {
        authId: current.authId,
      });
      if (!accepted) throw new Error("本次认证挑战已过期或已被处理。");
      setResponses([]);
      removeCurrent();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <dialog
      ref={dialogRef}
      className="server-modal auth-challenge-modal"
      aria-labelledby="auth-challenge-title"
      onCancel={(event) => {
        event.preventDefault();
        void cancel();
      }}
    >
      {current ? (
        <form className="server-form" onSubmit={(event) => void submit(event)}>
          <header className="modal-header">
            <div>
              <p className="eyebrow">SSH 二次认证</p>
              <h2 id="auth-challenge-title">{current.name || "服务器身份验证"}</h2>
            </div>
            <span className="auth-challenge-target" title={`${current.username}@${current.host}`}>
              <Icon name="shield" size="sm" />
              {current.username}@{current.host}
            </span>
          </header>

          <fieldset className="server-form-fields" disabled={busy}>
            {current.instructions ? (
              <p className="auth-challenge-instructions">{current.instructions}</p>
            ) : null}
            {current.prompts.map((prompt, index) => (
              <div className="form-field" key={`${current.authId}-${index}`}>
                <label className="field-label" htmlFor={`auth-response-${index}`}>
                  {prompt.prompt || `验证信息 ${index + 1}`}
                </label>
                <input
                  id={`auth-response-${index}`}
                  data-auth-input
                  className="form-input"
                  type={prompt.echo ? "text" : "password"}
                  value={responses[index] ?? ""}
                  autoComplete="off"
                  spellCheck={false}
                  onChange={(event) => {
                    const value = event.target.value;
                    setResponses((current) =>
                      current.map((response, currentIndex) =>
                        currentIndex === index ? value : response,
                      ),
                    );
                  }}
                />
              </div>
            ))}
            {error ? <p className="form-error" role="alert">{error}</p> : null}
          </fieldset>

          <div className="modal-actions">
            <button type="button" className="button-secondary" disabled={busy} onClick={() => void cancel()}>
              取消连接
            </button>
            <button type="submit" className="button-primary" disabled={busy}>
              {busy ? "正在验证…" : "继续验证"}
            </button>
          </div>
        </form>
      ) : null}
    </dialog>
  );
}

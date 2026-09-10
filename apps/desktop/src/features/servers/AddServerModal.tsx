import { useMutation, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS, type Environment, type Server } from "@yukinal/shared";
import { useEffect, useRef, useState } from "react";

import { Icon } from "../../components/Icon.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { ENVIRONMENTS, ENVIRONMENT_LABEL_SHORT } from "../../lib/labels.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { buildServerInput } from "./server-form.js";

export function AddServerModal({ onClose, server }: { onClose: () => void; server?: Server }) {
  const queryClient = useQueryClient();
  const selectServer = useWorkspaceStore((state) => state.selectServer);
  const dialogRef = useRef<HTMLDialogElement>(null);
  const nameInputRef = useRef<HTMLInputElement>(null);
  const [name, setName] = useState(server?.name ?? "");
  const [host, setHost] = useState(server?.connection.host ?? "");
  const [port, setPort] = useState(String(server?.connection.port ?? 22));
  const [username, setUsername] = useState(server?.connection.username ?? "");
  // Defaults to "unknown", never "staging": staging is the one non-obvious class
  // that lets the Agent auto-approve ordinary writes, so preselecting it means a
  // user who misses this field gets a mislabelled host *and* auto-approved writes
  // to it. "unknown" is honest and maps to the high risk floor.
  const [environment, setEnvironment] = useState<Environment>(server?.metadata.environment ?? "unknown");
  const [authMethod, setAuthMethod] = useState<"password" | "privateKey">("password");
  const [password, setPassword] = useState("");
  const [privateKeyPem, setPrivateKeyPem] = useState("");

  useEffect(() => {
    if (!server) return;
    setName(server.name);
    setHost(server.connection.host);
    setPort(String(server.connection.port));
    setUsername(server.connection.username);
    setEnvironment(server.metadata.environment);
  }, [server]);

  useEffect(() => {
    const previousFocus = document.activeElement;
    dialogRef.current?.showModal();
    const focusFrame = requestAnimationFrame(() => nameInputRef.current?.focus({ preventScroll: true }));
    return () => {
      cancelAnimationFrame(focusFrame);
      dialogRef.current?.close();
      if (previousFocus instanceof HTMLElement) previousFocus.focus();
    };
  }, []);

  const save = useMutation<{ server: { id: string } }, Error>({
    mutationFn: () => {
      const values = { name, host, port, username, environment, authMethod, password, privateKeyPem };
      return server
        ? callDesktop(IPC_COMMANDS.serverUpdate, buildServerInput(values, server.id))
        : callDesktop(IPC_COMMANDS.serverAdd, buildServerInput(values));
    },
    onSuccess: (response) => {
      void queryClient.invalidateQueries({ queryKey: ["servers"] });
      selectServer(response.server.id);
      onClose();
    },
  });

  return (
      <dialog ref={dialogRef} className="server-modal" aria-labelledby="server-modal-title" onCancel={(event) => { event.preventDefault(); if (!save.isPending) onClose(); }}>
        <header className="modal-header">
          <div>
            <p className="eyebrow">服务器连接</p>
            <h2 id="server-modal-title">{server ? "编辑服务器" : "添加服务器"}</h2>
          </div>
          <button type="button" className="icon-button modal-close" aria-label="关闭弹窗" title="关闭" disabled={save.isPending} onClick={onClose}>
            <Icon name="close" size="md" />
          </button>
        </header>

        <form className="server-form" onSubmit={(event) => { event.preventDefault(); if (!save.isPending) save.mutate(); }}>
          {!isDesktopShell() ? <p className="settings-notice">连接配置需要在桌面应用中保存。</p> : null}
          <fieldset className="server-form-fields" disabled={save.isPending}>
          <div className="form-field field-wide">
            <label className="field-label" htmlFor="server-name">名称</label>
            <input ref={nameInputRef} id="server-name" className="form-input" value={name} onChange={(event) => setName(event.target.value)} placeholder="例如：生产 API" required />
          </div>

          <div className="form-grid form-grid-host">
            <div className="form-field">
              <label className="field-label" htmlFor="server-host">主机</label>
              <input id="server-host" className="form-input" value={host} onChange={(event) => setHost(event.target.value)} placeholder="api.example.com" required />
            </div>
            <div className="form-field">
              <label className="field-label" htmlFor="server-port">端口</label>
              <input id="server-port" type="number" min={1} max={65535} step={1} className="form-input form-input-mono" value={port} onChange={(event) => setPort(event.target.value)} inputMode="numeric" required />
            </div>
          </div>

          <div className="form-grid">
            <div className="form-field">
              <label className="field-label" htmlFor="server-username">用户名</label>
              <input id="server-username" className="form-input" value={username} onChange={(event) => setUsername(event.target.value)} placeholder="deploy" required />
            </div>
            <div className="form-field">
              <label className="field-label" htmlFor="server-environment">环境</label>
              <select id="server-environment" className="form-input" value={environment} onChange={(event) => setEnvironment(event.target.value as Environment)}>
                {ENVIRONMENTS.map((env) => <option key={env} value={env}>{ENVIRONMENT_LABEL_SHORT[env]}</option>)}
              </select>
            </div>
          </div>

          <fieldset className="form-fieldset">
            <legend className="field-label">认证方式</legend>
            <div className="form-radio-group">
              <label className="form-radio-option"><input type="radio" name="server-authentication" checked={authMethod === "password"} onChange={() => setAuthMethod("password")} />密码</label>
              <label className="form-radio-option"><input type="radio" name="server-authentication" checked={authMethod === "privateKey"} onChange={() => setAuthMethod("privateKey")} />SSH 私钥</label>
            </div>
          </fieldset>

          {authMethod === "password" ? (
            <div className="form-field">
              <label className="field-label" htmlFor="server-password">密码{server ? "（留空保留现有认证）" : ""}</label>
              <input id="server-password" className="form-input" type="password" value={password} onChange={(event) => setPassword(event.target.value)} autoComplete="new-password" required={!server} />
            </div>
          ) : (
            <div className="form-field">
              <label className="field-label" htmlFor="server-private-key">私钥 PEM{server ? "（留空保留现有认证）" : ""}</label>
              <textarea id="server-private-key" className="form-input form-textarea" value={privateKeyPem} onChange={(event) => setPrivateKeyPem(event.target.value)} placeholder="-----BEGIN OPENSSH PRIVATE KEY-----" required={!server} spellCheck={false} />
              <p className="form-hint">仅保存到系统凭据库。暂不支持加密私钥。</p>
            </div>
          )}
          </fieldset>

          {save.isError ? <p className="form-error" role="alert">{save.error.message}</p> : null}

          <div className="modal-actions">
            <button type="button" disabled={save.isPending} onClick={onClose} className="button-secondary">取消</button>
            <button type="submit" disabled={!isDesktopShell() || save.isPending || !name.trim() || !host.trim() || !username.trim()} className="button-primary">
              {save.isPending ? "保存中…" : server ? "保存修改" : "保存服务器"}
            </button>
          </div>
        </form>
      </dialog>
  );
}

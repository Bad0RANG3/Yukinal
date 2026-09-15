import { useMutation, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS, type Environment, type Server } from "@yukinal/shared";
import { useEffect, useRef, useState } from "react";

import { Icon } from "../../components/Icon.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { ENVIRONMENTS, ENVIRONMENT_LABEL_SHORT } from "../../lib/labels.js";
import { SERVERS_QUERY_KEY } from "../../lib/servers.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { HostKeySection } from "./HostKeySection.js";
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
  const [authMethod, setAuthMethod] = useState<"password" | "privateKey" | "certificate" | "agent">("password");
  const [password, setPassword] = useState("");
  const [privateKeyPem, setPrivateKeyPem] = useState("");
  const [passphrase, setPassphrase] = useState("");
  const [certificatePath, setCertificatePath] = useState("");
  const [privateKeyPath, setPrivateKeyPath] = useState("");
  const [hostCertificateEnabled, setHostCertificateEnabled] = useState(Boolean(server?.connection.hostCertificateAuthority));
  const [hostCertificateConfigured, setHostCertificateConfigured] = useState(Boolean(server?.connection.hostCertificateAuthority));
  const [hostCaPublicKey, setHostCaPublicKey] = useState(server?.connection.hostCertificateAuthority?.caPublicKey ?? "");
  const [hostPrincipals, setHostPrincipals] = useState(server?.connection.hostCertificateAuthority?.principals.join("\n") ?? "");
  const [hostRevocationListPath, setHostRevocationListPath] = useState(server?.connection.hostCertificateAuthority?.revocationListPath ?? "");
  const [hostRevocationListUrl, setHostRevocationListUrl] = useState(server?.connection.hostCertificateAuthority?.revocationListUrl ?? "");
  const [hostRevocationListSigners, setHostRevocationListSigners] = useState(
    server?.connection.hostCertificateAuthority?.revocationListSigners?.join("\n") ?? "",
  );

  useEffect(() => {
    if (!server) return;
    setName(server.name);
    setHost(server.connection.host);
    setPort(String(server.connection.port));
    setUsername(server.connection.username);
    setEnvironment(server.metadata.environment);
    setHostCertificateEnabled(Boolean(server.connection.hostCertificateAuthority));
    setHostCertificateConfigured(Boolean(server.connection.hostCertificateAuthority));
    setHostCaPublicKey(server.connection.hostCertificateAuthority?.caPublicKey ?? "");
    setHostPrincipals(server.connection.hostCertificateAuthority?.principals.join("\n") ?? "");
    setHostRevocationListPath(server.connection.hostCertificateAuthority?.revocationListPath ?? "");
    setHostRevocationListUrl(server.connection.hostCertificateAuthority?.revocationListUrl ?? "");
    setHostRevocationListSigners(
      server.connection.hostCertificateAuthority?.revocationListSigners?.join("\n") ?? "",
    );
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
      const values = {
        name,
        host,
        port,
        username,
        environment,
        authMethod,
        password,
        privateKeyPem,
        passphrase,
        certificatePath,
        privateKeyPath,
        hostCertificateEnabled,
        hostCertificateConfigured,
        hostCaPublicKey,
        hostPrincipals,
        hostRevocationListPath,
        hostRevocationListUrl,
        hostRevocationListSigners,
      };
      return server
        ? callDesktop(IPC_COMMANDS.serverUpdate, buildServerInput(values, server.id))
        : callDesktop(IPC_COMMANDS.serverAdd, buildServerInput(values));
    },
    onSuccess: (response) => {
      void queryClient.invalidateQueries({ queryKey: SERVERS_QUERY_KEY });
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

          <fieldset className="form-fieldset" disabled={save.isPending}>
            <legend className="field-label">认证方式</legend>
            <div className="form-radio-group">
              <label className="form-radio-option"><input type="radio" name="server-authentication" checked={authMethod === "password"} onChange={() => setAuthMethod("password")} />密码</label>
              <label className="form-radio-option"><input type="radio" name="server-authentication" checked={authMethod === "privateKey"} onChange={() => setAuthMethod("privateKey")} />SSH 私钥</label>
              <label className="form-radio-option"><input type="radio" name="server-authentication" checked={authMethod === "certificate"} onChange={() => setAuthMethod("certificate")} />SSH 证书</label>
              <label className="form-radio-option"><input type="radio" name="server-authentication" checked={authMethod === "agent"} onChange={() => setAuthMethod("agent")} />SSH Agent</label>
            </div>
          </fieldset>

          {authMethod === "password" ? (
            <div className="form-field">
              <label className="field-label" htmlFor="server-password">密码{server ? "（留空保留现有认证）" : ""}</label>
              <input id="server-password" className="form-input" type="password" value={password} onChange={(event) => setPassword(event.target.value)} autoComplete="new-password" required={!server} />
            </div>
          ) : authMethod === "privateKey" ? (
            <>
              <div className="form-field">
                <label className="field-label" htmlFor="server-private-key">私钥 PEM{server ? "（留空保留现有认证）" : ""}</label>
                <textarea id="server-private-key" className="form-input form-textarea" value={privateKeyPem} onChange={(event) => setPrivateKeyPem(event.target.value)} placeholder="-----BEGIN OPENSSH PRIVATE KEY-----" required={!server} spellCheck={false} />
              </div>
              <div className="form-field">
                <label className="field-label" htmlFor="server-passphrase">口令（可选）</label>
                <input id="server-passphrase" className="form-input" type="password" value={passphrase} onChange={(event) => setPassphrase(event.target.value)} autoComplete="new-password" />
                <p className="form-hint">明文私钥留空即可；加密私钥请填写它的口令。口令单独保存在系统凭据库中。</p>
              </div>
              <p className="form-hint">私钥与口令都只保存到系统凭据库，不会写入数据库或上传。</p>
            </>
          ) : authMethod === "certificate" ? (
            <>
              <div className="form-field">
                <label className="field-label" htmlFor="server-certificate-key">私钥 PEM</label>
                <textarea id="server-certificate-key" className="form-input form-textarea" value={privateKeyPem} onChange={(event) => setPrivateKeyPem(event.target.value)} placeholder="-----BEGIN OPENSSH PRIVATE KEY-----" required spellCheck={false} />
              </div>
              <div className="form-field">
                <label className="field-label" htmlFor="server-certificate-path">证书路径</label>
                <input id="server-certificate-path" className="form-input form-input-mono" value={certificatePath} onChange={(event) => setCertificatePath(event.target.value)} placeholder="~/.ssh/id_ed25519-cert.pub" required spellCheck={false} />
              </div>
              <div className="form-field">
                <label className="field-label" htmlFor="server-private-key-path">私钥路径（可选）</label>
                <input id="server-private-key-path" className="form-input form-input-mono" value={privateKeyPath} onChange={(event) => setPrivateKeyPath(event.target.value)} placeholder="~/.ssh/id_ed25519" spellCheck={false} />
              </div>
              <div className="form-field">
                <label className="field-label" htmlFor="server-certificate-passphrase">口令（可选）</label>
                <input id="server-certificate-passphrase" className="form-input" type="password" value={passphrase} onChange={(event) => setPassphrase(event.target.value)} autoComplete="new-password" />
                <p className="form-hint">证书是公开文件，按路径读取；私钥与口令只保存在系统凭据库。证书必须认证当前提供的这把私钥。</p>
              </div>
            </>
          ) : (
            <div className="form-field">
              <p className="form-hint">使用本机正在运行的 ssh-agent 中的身份，不保存任何密码或私钥。请先用 ssh-add 把密钥加载进 agent；agent 不可用时会明确报错，不会退回其他认证方式。</p>
            </div>
          )}
          </fieldset>

          <fieldset className="form-fieldset" disabled={save.isPending}>
            <legend className="field-label">Host 证书信任（可选）</legend>
            <label className="form-check-option">
              <input
                type="checkbox"
                checked={hostCertificateEnabled}
                onChange={(event) => setHostCertificateEnabled(event.target.checked)}
              />
              要求服务器出示由指定 CA 签发的 host 证书
            </label>
            {hostCertificateEnabled ? (
              <>
                <div className="form-field">
                  <label className="field-label" htmlFor="server-host-ca">CA 公钥</label>
                  <textarea
                    id="server-host-ca"
                    className="form-input form-textarea form-textarea-compact"
                    value={hostCaPublicKey}
                    onChange={(event) => setHostCaPublicKey(event.target.value)}
                    placeholder="ssh-ed25519 AAAA... host-ca"
                    spellCheck={false}
                    required
                  />
                </div>
                <div className="form-field">
                  <label className="field-label" htmlFor="server-host-principals">允许的 principals</label>
                  <textarea
                    id="server-host-principals"
                    className="form-input form-textarea form-textarea-compact"
                    value={hostPrincipals}
                    onChange={(event) => setHostPrincipals(event.target.value)}
                    placeholder={"*.example.com\napi.internal"}
                    spellCheck={false}
                    required
                  />
                  <p className="form-hint">逗号或换行分隔；支持 `*` 与 `?`。启用后普通 host key 会被拒绝。</p>
                </div>
                <div className="form-field">
                  <label className="field-label" htmlFor="server-host-krl">KRL 路径（可选）</label>
                  <input
                    id="server-host-krl"
                    className="form-input form-input-mono"
                    value={hostRevocationListPath}
                    onChange={(event) => setHostRevocationListPath(event.target.value)}
                    placeholder="/etc/ssh/revoked_hosts.krl"
                    spellCheck={false}
                    disabled={hostRevocationListUrl.trim().length > 0}
                  />
                  <p className="form-hint">连接时读取本机 OpenSSH KRL；证书序列号、key ID、CA 或公钥撤销都会拒绝连接。</p>
                </div>
                <div className="form-field">
                  <label className="field-label" htmlFor="server-host-krl-url">KRL HTTPS URL（可选）</label>
                  <input
                    id="server-host-krl-url"
                    className="form-input form-input-mono"
                    value={hostRevocationListUrl}
                    onChange={(event) => setHostRevocationListUrl(event.target.value)}
                    placeholder="https://ca.example.com/revoked_hosts.krl"
                    spellCheck={false}
                    disabled={hostRevocationListPath.trim().length > 0}
                  />
                  <p className="form-hint">只接受 HTTPS，且 URL 不能内嵌凭据、query 或 fragment；下载失败时拒绝连接，不会退回旧列表。</p>
                </div>
                <div className="form-field field-wide">
                  <label className="field-label" htmlFor="server-host-krl-signers">
                    KRL 独立签名公钥（可选，一行一个）
                  </label>
                  <textarea
                    id="server-host-krl-signers"
                    className="form-input form-textarea form-textarea-compact form-input-mono"
                    value={hostRevocationListSigners}
                    onChange={(event) => setHostRevocationListSigners(event.target.value)}
                    placeholder={"ssh-ed25519 AAAA... krl-signer-2026-a\nssh-ed25519 AAAA... krl-signer-2026-b"}
                    spellCheck={false}
                  />
                  <p className="form-hint">
                    最多 8 把 OpenSSH 公钥。Host CA 始终受信；KRL 含有有效签名时，必须至少由 CA
                    或其中一把签名。轮换期间可同时列出旧、新密钥。
                  </p>
                </div>
              </>
            ) : null}
          </fieldset>

          {save.isError ? <p className="form-error" role="alert">{save.error.message}</p> : null}

          {server ? (
            // 指纹面板只挂在「编辑已有服务器」这一侧：它操作的是**已保存**的主机与端口
            // （命令按 serverId 去数据库里取），新建的服务器还没有对象可用。
            //
            // 于是有一个需要说清楚的错位：用户在这里改了主机名、还没保存时，下面那一栏
            // 描述的仍是旧主机。面板自己把那台主机:端口 显示在标题旁边，并写明「指纹按
            // 主机:端口 记录，不按服务器条目记录」，所以看到的那一刻就能对上号 —— 这也
            // 正是它不能把端点藏起来的理由。
            <HostKeySection
              serverId={server.id}
              caPolicyEnabled={Boolean(server.connection.hostCertificateAuthority)}
            />
          ) : null}

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

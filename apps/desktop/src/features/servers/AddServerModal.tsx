/**
 * 添加服务器表单（spec §77 的 MVP 形态）。
 *
 * 提交后 secret（密码 / 私钥）走 `server_add` → OS keychain；SQLite 只存引用。
 * 高级配置（ProxyJump/AgentForwarding/Keepalive/KnownHosts）刻意隐藏。
 */

import { useMutation, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS, type AddServerInput, type Environment } from "@yukinal/shared";
import { useState } from "react";

import { callDesktop } from "../../lib/ipc.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";

const ENVIRONMENTS: Environment[] = ["local", "development", "staging", "production", "unknown"];

export function AddServerModal({ onClose }: { onClose: () => void }) {
  const queryClient = useQueryClient();
  const selectServer = useWorkspaceStore((state) => state.selectServer);

  const [name, setName] = useState("");
  const [host, setHost] = useState("");
  const [port, setPort] = useState("22");
  const [username, setUsername] = useState("");
  const [environment, setEnvironment] = useState<Environment>("staging");
  const [authMethod, setAuthMethod] = useState<"password" | "privateKey">("password");
  const [password, setPassword] = useState("");
  const [privateKeyPem, setPrivateKeyPem] = useState("");

  const add = useMutation<{ server: { id: string } }, Error>({
    mutationFn: () => {
      const input: AddServerInput = {
        name: name.trim(),
        host: host.trim(),
        port: parseInt(port, 10) || 22,
        username: username.trim(),
        environment,
        authentication:
          authMethod === "password"
            ? { method: "password", password }
            : { method: "privateKey", privateKeyPem: privateKeyPem.trim() },
      };
      return callDesktop(IPC_COMMANDS.serverAdd, input);
    },
    onSuccess: (response) => {
      void queryClient.invalidateQueries({ queryKey: ["servers"] });
      selectServer(response.server.id);
      onClose();
    },
  });

  const input = "w-full rounded-md border border-zinc-800 bg-zinc-900 p-2 text-sm outline-none focus:border-zinc-600 placeholder:text-zinc-600";
  const label = "mb-1 block text-xs text-zinc-400";

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60" onClick={onClose}>
      <div
        className="w-full max-w-md rounded-lg border border-zinc-800 bg-zinc-950 p-5 shadow-xl"
        onClick={(event) => event.stopPropagation()}
      >
        <h2 className="mb-4 text-base font-semibold">添加服务器</h2>

        <div className="space-y-3">
          <div>
            <label className={label}>名称</label>
            <input className={input} value={name} onChange={(event) => setName(event.target.value)} placeholder="生产 API" />
          </div>
          <div className="grid grid-cols-3 gap-3">
            <div className="col-span-2">
              <label className={label}>主机</label>
              <input className={input} value={host} onChange={(event) => setHost(event.target.value)} placeholder="api.example.com" />
            </div>
            <div>
              <label className={label}>端口</label>
              <input className={input} value={port} onChange={(event) => setPort(event.target.value)} placeholder="22" />
            </div>
          </div>
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className={label}>用户名</label>
              <input className={input} value={username} onChange={(event) => setUsername(event.target.value)} placeholder="deploy" />
            </div>
            <div>
              <label className={label}>环境</label>
              <select className={input} value={environment} onChange={(event) => setEnvironment(event.target.value as Environment)}>
                {ENVIRONMENTS.map((env) => (
                  <option key={env} value={env}>
                    {env}
                  </option>
                ))}
              </select>
            </div>
          </div>

          <div>
            <label className={label}>认证方式</label>
            <div className="flex gap-4 text-sm">
              <label className="flex items-center gap-1.5">
                <input type="radio" checked={authMethod === "password"} onChange={() => setAuthMethod("password")} />
                密码
              </label>
              <label className="flex items-center gap-1.5">
                <input type="radio" checked={authMethod === "privateKey"} onChange={() => setAuthMethod("privateKey")} />
                SSH 私钥
              </label>
            </div>
          </div>

          {authMethod === "password" ? (
            <div>
              <label className={label}>密码</label>
              <input className={input} type="password" value={password} onChange={(event) => setPassword(event.target.value)} />
            </div>
          ) : (
            <div>
              <label className={label}>私钥（PEM，OpenSSH 格式）</label>
              <textarea
                className={`${input} h-24 resize-none font-mono text-xs`}
                value={privateKeyPem}
                onChange={(event) => setPrivateKeyPem(event.target.value)}
                placeholder="-----BEGIN OPENSSH PRIVATE KEY-----"
              />
              <p className="mt-1 text-[11px] text-zinc-600">私钥只会进 OS keychain，绝不写进 SQLite；加密私钥暂不支持。</p>
            </div>
          )}

          {add.isError ? <p className="text-sm text-red-400">{add.error.message}</p> : null}
        </div>

        <div className="mt-5 flex justify-end gap-2">
          <button type="button" onClick={onClose} className="rounded-md border border-zinc-700 px-3 py-1.5 text-sm text-zinc-300">
            取消
          </button>
          <button
            type="button"
            disabled={add.isPending || !name.trim() || !host.trim() || !username.trim()}
            onClick={() => add.mutate()}
            className="rounded-md bg-zinc-100 px-3 py-1.5 text-sm font-medium text-zinc-900 disabled:opacity-40"
          >
            {add.isPending ? "保存中…" : "保存并连接"}
          </button>
        </div>
      </div>
    </div>
  );
}
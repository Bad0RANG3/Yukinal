import { useMutation, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS, type AddServerInput, type Environment, type Server, type UpdateServerInput } from "@yukinal/shared";
import { useEffect, useState } from "react";

import { callDesktop } from "../../lib/ipc.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";

const ENVIRONMENTS: Environment[] = ["local", "development", "staging", "production", "unknown"];

export function AddServerModal({ onClose, server }: { onClose: () => void; server?: Server }) {
  const queryClient = useQueryClient();
  const selectServer = useWorkspaceStore((state) => state.selectServer);
  const [name, setName] = useState(server?.name ?? "");
  const [host, setHost] = useState(server?.connection.host ?? "");
  const [port, setPort] = useState(String(server?.connection.port ?? 22));
  const [username, setUsername] = useState(server?.connection.username ?? "");
  const [environment, setEnvironment] = useState<Environment>(server?.metadata.environment ?? "staging");
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

  const save = useMutation<{ server: { id: string } }, Error>({
    mutationFn: () => {
      const base = { name: name.trim(), host: host.trim(), port: Number.parseInt(port, 10) || 22, username: username.trim(), environment };
      const authentication = authMethod === "password" ? { method: "password" as const, password } : { method: "privateKey" as const, privateKeyPem: privateKeyPem.trim() };
      if (server) {
        const input: UpdateServerInput = { ...base, serverId: server.id };
        if (password.trim() || privateKeyPem.trim()) input.authentication = authentication;
        return callDesktop(IPC_COMMANDS.serverUpdate, input);
      }
      const input: AddServerInput = { ...base, authentication };
      return callDesktop(IPC_COMMANDS.serverAdd, input);
    },
    onSuccess: (response) => { void queryClient.invalidateQueries({ queryKey: ["servers"] }); selectServer(response.server.id); onClose(); },
  });

  const inputClass = "w-full rounded-md border border-zinc-800 bg-zinc-900 p-2 text-sm outline-none focus:border-zinc-600 placeholder:text-zinc-600";
  const labelClass = "mb-1 block text-xs text-zinc-400";
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60" onClick={onClose}>
      <div className="w-full max-w-md rounded-lg border border-zinc-800 bg-zinc-950 p-5 shadow-xl" onClick={(event) => event.stopPropagation()}>
        <h2 className="mb-4 text-base font-semibold">{server ? "Edit server" : "Add server"}</h2>
        <div className="space-y-3">
          <div><label className={labelClass}>Name</label><input className={inputClass} value={name} onChange={(event) => setName(event.target.value)} placeholder="Production API" /></div>
          <div className="grid grid-cols-3 gap-3"><div className="col-span-2"><label className={labelClass}>Host</label><input className={inputClass} value={host} onChange={(event) => setHost(event.target.value)} placeholder="api.example.com" /></div><div><label className={labelClass}>Port</label><input className={inputClass} value={port} onChange={(event) => setPort(event.target.value)} /></div></div>
          <div className="grid grid-cols-2 gap-3"><div><label className={labelClass}>Username</label><input className={inputClass} value={username} onChange={(event) => setUsername(event.target.value)} placeholder="deploy" /></div><div><label className={labelClass}>Environment</label><select className={inputClass} value={environment} onChange={(event) => setEnvironment(event.target.value as Environment)}>{ENVIRONMENTS.map((env) => <option key={env} value={env}>{env}</option>)}</select></div></div>
          <div><label className={labelClass}>Authentication</label><div className="flex gap-4 text-sm"><label className="flex items-center gap-1.5"><input type="radio" checked={authMethod === "password"} onChange={() => setAuthMethod("password")} />Password</label><label className="flex items-center gap-1.5"><input type="radio" checked={authMethod === "privateKey"} onChange={() => setAuthMethod("privateKey")} />SSH private key</label></div></div>
          {authMethod === "password" ? <div><label className={labelClass}>Password{server ? " (blank keeps current)" : ""}</label><input className={inputClass} type="password" value={password} onChange={(event) => setPassword(event.target.value)} /></div> : <div><label className={labelClass}>Private key PEM{server ? " (blank keeps current)" : ""}</label><textarea className={`${inputClass} h-24 resize-none font-mono text-xs`} value={privateKeyPem} onChange={(event) => setPrivateKeyPem(event.target.value)} placeholder="-----BEGIN OPENSSH PRIVATE KEY-----" /><p className="mt-1 text-[11px] text-zinc-600">Stored only in the OS keychain. Encrypted keys are not supported yet.</p></div>}
          {save.isError ? <p className="text-sm text-red-400">{save.error.message}</p> : null}
        </div>
        <div className="mt-5 flex justify-end gap-2"><button type="button" onClick={onClose} className="rounded-md border border-zinc-700 px-3 py-1.5 text-sm text-zinc-300">Cancel</button><button type="button" disabled={save.isPending || !name.trim() || !host.trim() || !username.trim()} onClick={() => save.mutate()} className="rounded-md bg-zinc-100 px-3 py-1.5 text-sm font-medium text-zinc-900 disabled:opacity-40">{save.isPending ? "Saving..." : server ? "Save changes" : "Save and connect"}</button></div>
      </div>
    </div>
  );
}

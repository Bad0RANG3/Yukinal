import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { IPC_COMMANDS } from "@yukinal/shared";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useProviders } from "../../lib/providers.js";
import { useServers, useServerAction } from "../../lib/servers.js";
import { useAgentStatus, useSpawnAgent } from "../../lib/runtime.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { usePreferencesStore } from "../../stores/preferences-store.js";
import { ProviderEditor } from "../settings/RuntimeSettings.js";
import { AddServerModal } from "../servers/AddServerModal.js";

export function GettingStarted({ onClose }: { onClose(): void }) {
  const shell = isDesktopShell();
  const providers = useProviders();
  const servers = useServers();
  const runtime = useAgentStatus();
  const spawn = useSpawnAgent();
  const client = useQueryClient();
  const workspace = useWorkspaceStore();
  const [step, setStep] = useState(0);
  const [providerId, setProviderId] = useState<string | null>(null);
  const [creatingProvider, setCreatingProvider] = useState(false);
  const [editingProvider, setEditingProvider] = useState(false);
  const [serverModal, setServerModal] = useState<"add" | "edit" | null>(null);
  const provider = creatingProvider ? undefined : providers.data?.find((item) => item.id === providerId)
    ?? providers.data?.find((item) => item.enabled);
  const server = servers.data?.find((item) => item.id === workspace.selectedServerId);
  const { action, busy } = useServerAction();
  const test = useMutation({
    mutationFn: async (candidate: NonNullable<typeof provider>) => {
      await callDesktop(IPC_COMMANDS.providerTest, { providerId: candidate.id });
      return `${candidate.id}@${candidate.updatedAt}`;
    },
  });
  const tested = Boolean(provider && test.data === `${provider.id}@${provider.updatedAt}`);
  const connected = server?.status === "connected";
  const prepare = useMutation({
    mutationFn: async () => {
      if (!tested || !provider || !connected || !server) throw new Error("请先测试模型并连接服务器。");
      if (useWorkspaceStore.getState().agentBusy || useWorkspaceStore.getState().agentDraft.trim()) throw new Error("请先完成当前任务或清空 Agent 草稿。");
      await callDesktop(IPC_COMMANDS.providerActivate, { providerId: provider.id });
      await client.invalidateQueries({ queryKey: ["providers"] });
      const current = useWorkspaceStore.getState();
      if (current.agentBusy || current.agentDraft.trim()) throw new Error("请先完成当前任务或清空 Agent 草稿，再准备首次排查。");
      current.selectProvider(provider.id, provider.model);
      current.selectServer(server.id);
      current.setAgentOpen(true);
      current.setAgentDraft("请对当前服务器做一次只读健康巡检：查看资源使用情况和容器状态，必要时读取容器日志；总结异常、依据和建议。不要修改文件或重启服务。");
      usePreferencesStore.getState().setPreferences({ agentPermissionMode: "ask" });
      onClose();
      requestAnimationFrame(() => document.querySelector<HTMLTextAreaElement>("#agent-panel textarea")?.focus());
    },
  });
  const changeStep = (next: number) => { setStep(next); prepare.reset(); };

  return (
    <section className="getting-started" aria-labelledby="getting-started-title">
      <header className="getting-started-header">
        <div><p className="eyebrow">欢迎使用 YUKINAL</p><h2 id="getting-started-title">从第一次排查开始</h2><p>连接模型与服务器，让 Agent 带着真实上下文协助你。</p></div>
        <button className="button-secondary" onClick={onClose}>稍后设置</button>
      </header>
      <ol className="getting-started-steps" aria-label="首次使用步骤">
        {["配置并测试模型", "连接 SSH 服务器", "开始第一次排查"].map((label, index) => (
          <li key={label} aria-current={step === index ? "step" : undefined}><span>0{index + 1}</span>{label}</li>
        ))}
      </ol>
      {!shell ? <p className="settings-notice" role="status">当前为浏览器预览。模型测试和 SSH 连接需要在 Yukinal 桌面应用中进行。</p> : null}
      {(providers.isLoading || servers.isLoading) && shell ? <p role="status">正在读取已有配置…</p> : null}
      {providers.isError || servers.isError ? <div role="alert" className="form-error">无法读取已有配置。<button className="text-button" onClick={() => { void providers.refetch(); void servers.refetch(); }}>重新加载</button></div> : null}
      {step === 0 ? <>
        <section className="settings-card">
          <h3>选择模型连接</h3>
          <p>可以使用已有配置，也可以添加一个兼容端点。测试会发送一条简短消息，可能产生少量模型费用。</p>
          <label className="settings-field"><span className="field-label">模型连接</span>
            <select className="form-input" disabled={!shell || test.isPending || workspace.agentBusy} value={creatingProvider ? "" : provider?.id ?? ""} onChange={(event) => {
              setProviderId(event.target.value || null); setCreatingProvider(!event.target.value); setEditingProvider(false); test.reset();
            }}>
              <option value="">添加新的模型连接</option>
              {providers.data?.map((item) => <option value={item.id} key={item.id}>{item.label} · {item.model}</option>)}
            </select>
          </label>
          {provider ? <div className="settings-actions"><button className="button-secondary" disabled={test.isPending} onClick={() => { setEditingProvider((value) => !value); test.reset(); }}>编辑配置</button></div> : null}
        </section>
        {(!provider || editingProvider) && !providers.isLoading ? <ProviderEditor key={provider?.id ?? "new"} provider={provider} onSaved={({ provider: saved }) => {
          client.setQueryData(["providers"], (current: typeof providers.data) => [...(current ?? []).filter((item) => item.id !== saved.id), saved]);
          void client.invalidateQueries({ queryKey: ["providers"] });
          setProviderId(saved.id); setCreatingProvider(false); setEditingProvider(false); test.reset();
        }} /> : null}
        <section className="settings-card">
          <h3>测试模型回复</h3><p>使用已保存的端点、密钥和模型，验证实际生成能力。编辑配置后请重新测试。</p>
          {shell && !runtime.data?.running ? <div className="settings-actions"><span>Agent 尚未就绪。</span><button className="button-secondary" disabled={spawn.isPending} onClick={() => spawn.mutate()}>{spawn.isPending ? "启动中…" : "启动 Agent"}</button></div> : null}
          {spawn.isError ? <p className="form-error" role="alert">{spawn.error.message}</p> : null}
          <button className="button-primary" disabled={!shell || !provider || editingProvider || test.isPending || !runtime.data?.running || workspace.agentBusy} onClick={() => provider && test.mutate(provider)}>{test.isPending ? "等待模型回复（最多 35 秒）…" : "测试模型连接"}</button>
          {test.isError ? <p className="form-error" role="alert">{test.error.message}</p> : null}
          {tested ? <p className="form-success" role="status">模型已成功返回文本回复，可以继续。</p> : null}
        </section>
      </> : null}
      {step === 1 ? <section className="settings-card">
        <h3>连接你的服务器</h3><p>选择已有服务器，或添加 SSH 地址和认证信息。连接成功后才会进入排查步骤。</p>
        <label className="settings-field"><span className="field-label">服务器</span><select className="form-input" value={server?.id ?? ""} disabled={!shell || busy || workspace.agentBusy} onChange={(event) => { workspace.selectServer(event.target.value || null); action.reset(); }}><option value="">请选择服务器</option>{servers.data?.map((item) => <option key={item.id} value={item.id}>{item.name} · {item.connection.host}</option>)}</select></label>
        <p className="form-hint">首次连接会信任并保存主机指纹。请先通过可信渠道核验目标主机；后续指纹变化会拒绝连接。</p>
        <div className="settings-actions">
          <button className="button-secondary" disabled={!shell || busy} onClick={() => setServerModal("add")}>添加服务器</button>
          {server ? <button className="button-secondary" disabled={busy} onClick={() => setServerModal("edit")}>编辑连接</button> : null}
          <button className="button-primary" disabled={!shell || !server || busy || connected || workspace.agentBusy} onClick={() => server && action.mutate({ type: "connect", serverId: server.id })}>{busy ? "正在验证 SSH…" : connected ? "SSH 已连接" : "连接并验证 SSH"}</button>
        </div>
        {action.isError ? <p className="form-error" role="alert">{action.error.message} 请检查地址、端口、用户名及认证信息后重试。</p> : null}
        {connected ? <p className="form-success" role="status">已连接 {server.name}，SSH 验证成功。</p> : null}
      </section> : null}
      {step === 2 ? <section className="settings-card">
        <h3>准备一次只读巡检</h3><p>模型：{provider?.label} / {provider?.model}</p><p>服务器：{server?.name ?? "未选择"}</p>
        <p>Agent 将查看资源和容器状态，必要时读取容器日志，再给出异常依据和建议。任务会先填入右侧输入框，你可以修改后发送；操作权限设为“操作前询问”。</p>
        {!connected ? <p className="form-error" role="alert">服务器已断开，请返回上一步重新连接。</p> : null}
        {workspace.agentBusy || workspace.agentDraft.trim() ? <p className="settings-notice">请先完成当前 Agent 任务或清空已有草稿，以免覆盖正在进行的工作。</p> : null}
        {prepare.isError ? <p className="form-error" role="alert">{prepare.error.message}</p> : null}
      </section> : null}
      <footer className="getting-started-footer">
        <span>可随时从顶部“使用引导”重新打开。</span>
        <div className="settings-actions">
          {step > 0 ? <button className="button-secondary" disabled={prepare.isPending || busy} onClick={() => changeStep(step - 1)}>上一步</button> : null}
          {step < 2 ? <button className="button-primary" disabled={step === 0 ? !tested || editingProvider || test.isPending : !connected || busy} onClick={() => changeStep(step + 1)}>下一步</button>
            : <button className="button-primary" disabled={!tested || !connected || prepare.isPending || workspace.agentBusy || Boolean(workspace.agentDraft.trim())} onClick={() => prepare.mutate()}>{prepare.isPending ? "准备中…" : "填入首次排查任务"}</button>}
        </div>
      </footer>
      {serverModal ? <AddServerModal server={serverModal === "edit" ? server : undefined} onClose={() => { setServerModal(null); action.reset(); }} /> : null}
    </section>
  );
}

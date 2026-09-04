import { useQuery } from "@tanstack/react-query";
import {
  IPC_COMMANDS,
  WorkspaceListResponseSchema,
  type Environment,
  type Server,
  type Workspace,
} from "@yukinal/shared";

import { callDesktop, callDesktopParsed, isDesktopShell } from "../../lib/ipc.js";

const ENVIRONMENT_LABEL: Record<Environment, string> = {
  production: "生产",
  staging: "预发",
  development: "开发",
  local: "本地",
  unknown: "未知",
};

export function ProjectsPane() {
  const shell = isDesktopShell();
  const workspaces = useQuery({
    queryKey: ["workspaces"],
    enabled: shell,
    staleTime: 10_000,
    queryFn: async () =>
      callDesktopParsed(
        IPC_COMMANDS.workspaceList,
        {},
        (raw) => WorkspaceListResponseSchema.parse(raw),
      ),
  });
  const servers = useQuery({
    queryKey: ["servers"],
    enabled: shell,
    staleTime: 10_000,
    queryFn: async () => (await callDesktop(IPC_COMMANDS.serverList, {})).servers,
  });

  if (!shell) return <PreviewEmpty />;

  if (workspaces.isLoading) {
    return (
      <div className="loading-panel">
        <div className="loading-spinner" />
        <strong>正在读取项目</strong>
        <span>从本地 workspace 数据加载</span>
      </div>
    );
  }

  if (workspaces.isError || !workspaces.data) {
    return (
      <div className="error-panel">
        <div className="error-panel-icon">!</div>
        <div>
          <strong>无法读取项目</strong>
          <p>{workspaces.error instanceof Error ? workspaces.error.message : String(workspaces.error)}</p>
          <button type="button" className="secondary-button" onClick={() => void workspaces.refetch()}>
            重试
          </button>
        </div>
      </div>
    );
  }

  const serverNames = new Map((servers.data ?? []).map((server: Server) => [server.id, server.name]));
  const rows = workspaces.data.workspaces;

  return (
    <div className="projects-page">
      <section className="projects-page-header">
        <div>
          <p className="eyebrow">工作区集合</p>
          <h2>项目</h2>
          <p>按项目查看关联的服务器和代码仓库，目标环境始终明确可见。</p>
        </div>
        <button type="button" className="secondary-button" onClick={() => void workspaces.refetch()} disabled={workspaces.isFetching}>
          <span aria-hidden="true">↻</span> {workspaces.isFetching ? "读取中" : "刷新"}
        </button>
      </section>

      {rows.length === 0 ? (
        <div className="empty-state page-empty project-empty">
          <span className="empty-state-mark">⌁</span>
          <h2>暂无项目</h2>
          <p>本地数据库还没有 workspace 记录。服务器视图仍可独立使用。</p>
        </div>
      ) : (
        <ul className="project-grid" aria-label="项目列表">
          {rows.map((workspace) => <ProjectCard key={workspace.id} workspace={workspace} serverNames={serverNames} />)}
        </ul>
      )}
    </div>
  );
}

function ProjectCard({ workspace, serverNames }: { workspace: Workspace; serverNames: Map<string, string> }) {
  return (
    <li className="project-card">
      <div className="project-card-header">
        <div>
          <p className="eyebrow">Workspace</p>
          <h3>{workspace.name}</h3>
        </div>
        <span className={`project-environment project-environment-${workspace.defaultEnvironment}`}>
          {ENVIRONMENT_LABEL[workspace.defaultEnvironment]}
        </span>
      </div>
      <div className="project-card-meta">
        <span>{workspace.serverIds.length} 台服务器</span>
        <span>{workspace.repositories.length} 个仓库</span>
        <span>{workspace.providerIds.length} 个 Provider</span>
      </div>
      <div className="project-card-section">
        <span className="project-card-label">服务器</span>
        {workspace.serverIds.length ? (
          <ul className="project-server-list">
            {workspace.serverIds.map((serverId) => (
              <li key={serverId}><span className="project-resource-dot" />{serverNames.get(serverId) ?? serverId}</li>
            ))}
          </ul>
        ) : <span className="project-muted">未关联服务器</span>}
      </div>
      <div className="project-card-section">
        <span className="project-card-label">代码仓库</span>
        {workspace.repositories.length ? (
          <ul className="project-repository-list">
            {workspace.repositories.map((repository) => (
              <li key={repository.id}>
                <strong>{repository.name}</strong>
                <span>{repository.path ?? repository.gitUrl ?? (repository.host === "local" ? "本地仓库" : "远端仓库")}</span>
              </li>
            ))}
          </ul>
        ) : <span className="project-muted">未关联仓库</span>}
      </div>
    </li>
  );
}

function PreviewEmpty() {
  return (
    <div className="empty-state page-empty">
      <span className="empty-state-mark">◌</span>
      <h2>浏览器预览</h2>
      <p>本地 workspace 数据只在 Tauri 桌面壳中可用，预览不会伪造项目内容。</p>
    </div>
  );
}

import { useQuery } from "@tanstack/react-query";
import {
  IPC_COMMANDS,
  type Server,
  type Workspace,
} from "@yukinal/shared";

import { errorMessage } from "../../lib/format.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { ENVIRONMENT_LABEL_SHORT } from "../../lib/labels.js";
import { useServers } from "../../lib/servers.js";
import { EmptyPanel, ErrorPanel, LoadingPanel, PreviewEmpty } from "../../components/PanelStates.js";
import { Icon } from "../../components/Icon.js";
import { PageToolbar } from "../../components/PageToolbar.js";

export function ProjectsPane() {
  const shell = isDesktopShell();
  const workspaces = useQuery({
    queryKey: ["workspaces"],
    enabled: shell,
    staleTime: 10_000,
    queryFn: async () =>
      // `callDesktop` binds this command to the response schema registered in
      // `IPC_SCHEMAS`, so the command→schema pair has exactly one home.
      callDesktop(IPC_COMMANDS.workspaceList, {}),
  });
  // 与侧栏共用同一份服务器缓存（同一个 key、同一套新鲜度），不再是本地副本。
  const servers = useServers({ enabled: shell });

  if (!shell) return <PreviewEmpty icon="projects" body="桌面功能不可用" />;

  if (workspaces.isLoading) {
    return <LoadingPanel title="正在读取项目" />;
  }

  if (workspaces.isError || !workspaces.data) {
    return (
      <ErrorPanel
        showIcon
        title="无法读取项目"
        message={errorMessage(workspaces.error)}
        onRetry={() => void workspaces.refetch()}
      />
    );
  }

  const serverNames = new Map((servers.data ?? []).map((server: Server) => [server.id, server.name]));
  const rows = workspaces.data.workspaces;

  return (
    <div className="projects-page">
      <PageToolbar actions={
        <button type="button" className="button-secondary" onClick={() => void workspaces.refetch()} disabled={workspaces.isFetching} title="刷新项目" aria-label="刷新项目">
          <Icon name="refresh" size="sm" /> {workspaces.isFetching ? "读取中" : "刷新"}
        </button>
      }><span>{rows.length} 个项目</span></PageToolbar>

      {rows.length === 0 ? (
        <EmptyPanel extraClass="project-empty" icon="projects" title="暂无项目" />
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
          <h2>{workspace.name}</h2>
        </div>
        <span className={`project-environment project-environment-${workspace.defaultEnvironment}`}>
          {ENVIRONMENT_LABEL_SHORT[workspace.defaultEnvironment]}
        </span>
      </div>
      <div className="project-card-meta">
        <span>{workspace.serverIds.length} 台服务器</span>
        <span>{workspace.repositories.length} 个仓库</span>
        <span>{workspace.providerIds.length} 个连接</span>
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


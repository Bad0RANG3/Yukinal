import { useQuery } from "@tanstack/react-query";
import {
  IPC_COMMANDS,
  type ServiceSource,
  type ServiceState,
} from "@yukinal/shared";

import { errorMessage } from "../../lib/format.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { EmptyPanel, ErrorPanel, LoadingPanel, PreviewEmpty } from "../../components/PanelStates.js";
import { Icon } from "../../components/Icon.js";
import { KeywordText } from "../../components/KeywordText.js";

const STATE_LABEL: Record<ServiceState, string> = {
  running: "运行中",
  stopped: "已停止",
  failed: "失败",
  unknown: "未知",
};

const SOURCE_LABEL: Record<ServiceSource, string> = {
  systemd: "systemd 服务",
  docker: "Docker 容器",
  unavailable: "未发现服务管理器",
};

export function ServicesPane() {
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const shell = isDesktopShell();
  const servicesQuery = useQuery({
    queryKey: ["serverServices", selectedServerId],
    enabled: Boolean(selectedServerId) && shell,
    staleTime: 10_000,
    refetchInterval: 30_000,
    queryFn: async () => {
      if (!selectedServerId) return null;
      // `callDesktop` binds this command to the response schema registered in
      // `IPC_SCHEMAS`, so the command→schema pair has exactly one home.
      return callDesktop(IPC_COMMANDS.serverServices, { serverId: selectedServerId });
    },
  });

  if (!shell) return <PreviewEmpty icon="services" body="原生 SSH 能力只在 Tauri 桌面壳中可用，预览不会伪造远端服务数据。" />;

  if (!selectedServerId) {
    return <EmptyPanel icon="services" title="选择一台服务器" body="从左侧列表选择目标环境，查看远端服务状态。" />;
  }

  if (servicesQuery.isLoading) {
    return <LoadingPanel title="正在读取服务状态" hint="SSH · systemd / Docker · 预计几秒完成" />;
  }

  if (servicesQuery.isError || !servicesQuery.data) {
    return (
      <ErrorPanel
        showIcon
        title="无法读取服务状态"
        message={errorMessage(servicesQuery.error)}
        onRetry={() => void servicesQuery.refetch()}
      />
    );
  }

  const response = servicesQuery.data;
  const running = response.services.filter((service) => service.state === "running").length;
  const failed = response.services.filter((service) => service.state === "failed").length;

  return (
    <div className="services-page">
      <section className="services-page-header">
        <div>
          <p className="eyebrow">远程服务</p>
          <h2>服务</h2>
          <p>从目标服务器实时读取，不在本地猜测运行状态。</p>
        </div>
        <button type="button" className="button-secondary" onClick={() => void servicesQuery.refetch()} disabled={servicesQuery.isFetching} title="刷新服务" aria-label="刷新服务">
          <Icon name="refresh" size="sm" /> {servicesQuery.isFetching ? "读取中" : "刷新"}
        </button>
      </section>

      <div className="service-summary">
        <span className={`service-source service-source-${response.source}`}>{SOURCE_LABEL[response.source]}</span>
        <span>{response.services.length} 项 · {running} 项运行中{failed ? ` · ${failed} 项失败` : ""}</span>
      </div>

      {response.message ? <div className="service-notice">{response.message}</div> : null}

      {response.services.length === 0 ? (
        <EmptyPanel extraClass="service-empty" icon="services" title="没有可展示的服务" body={response.message ?? "服务管理器没有返回服务条目。"} />
      ) : (
        <ul className="service-list" aria-label="远程服务列表">
          {response.services.map((service) => (
            <li className="service-row" key={`${response.source}:${service.name}`}>
              <span className={`service-state-dot service-state-dot-${service.state}`} aria-hidden="true" />
              <div className="service-row-copy">
                <strong><KeywordText text={service.name} /></strong>
                <span><KeywordText text={service.description ?? "无描述"} /></span>
              </div>
              <code><KeywordText text={service.status} /></code>
              <span className={`service-state service-state-${service.state}`}>{STATE_LABEL[service.state]}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}


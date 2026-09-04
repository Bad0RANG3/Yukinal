import { useQuery } from "@tanstack/react-query";
import {
  IPC_COMMANDS,
  ServerServicesResponseSchema,
  type ServiceSource,
  type ServiceState,
} from "@yukinal/shared";

import { callDesktopParsed, isDesktopShell } from "../../lib/ipc.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";

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
      return callDesktopParsed(
        IPC_COMMANDS.serverServices,
        { serverId: selectedServerId },
        (raw) => ServerServicesResponseSchema.parse(raw),
      );
    },
  });

  if (!shell) return <PreviewEmpty />;

  if (!selectedServerId) {
    return (
      <div className="empty-state page-empty">
        <span className="empty-state-mark">◇</span>
        <h2>选择一台服务器</h2>
        <p>从左侧列表选择目标环境，查看远端服务状态。</p>
      </div>
    );
  }

  if (servicesQuery.isLoading) {
    return (
      <div className="loading-panel">
        <div className="loading-spinner" />
        <strong>正在读取服务状态</strong>
        <span>SSH · systemd / Docker · 预计几秒完成</span>
      </div>
    );
  }

  if (servicesQuery.isError || !servicesQuery.data) {
    return (
      <div className="error-panel">
        <div className="error-panel-icon">!</div>
        <div>
          <strong>无法读取服务状态</strong>
          <p>{servicesQuery.error instanceof Error ? servicesQuery.error.message : String(servicesQuery.error)}</p>
          <button type="button" className="secondary-button" onClick={() => void servicesQuery.refetch()}>
            重试
          </button>
        </div>
      </div>
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
        <button type="button" className="secondary-button" onClick={() => void servicesQuery.refetch()} disabled={servicesQuery.isFetching}>
          <span aria-hidden="true">↻</span> {servicesQuery.isFetching ? "读取中" : "刷新"}
        </button>
      </section>

      <div className="service-summary">
        <span className={`service-source service-source-${response.source}`}>{SOURCE_LABEL[response.source]}</span>
        <span>{response.services.length} 项 · {running} 项运行中{failed ? ` · ${failed} 项失败` : ""}</span>
      </div>

      {response.message ? <div className="service-notice">{response.message}</div> : null}

      {response.services.length === 0 ? (
        <div className="empty-state page-empty service-empty">
          <span className="empty-state-mark">◇</span>
          <h2>没有可展示的服务</h2>
          <p>{response.message ?? "服务管理器没有返回服务条目。"}</p>
        </div>
      ) : (
        <ul className="service-list" aria-label="远程服务列表">
          {response.services.map((service) => (
            <li className="service-row" key={`${response.source}:${service.name}`}>
              <span className={`service-state-dot service-state-dot-${service.state}`} aria-hidden="true" />
              <div className="service-row-copy">
                <strong>{service.name}</strong>
                <span>{service.description ?? "无描述"}</span>
              </div>
              <code>{service.status}</code>
              <span className={`service-state service-state-${service.state}`}>{STATE_LABEL[service.state]}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function PreviewEmpty() {
  return (
    <div className="empty-state page-empty">
      <span className="empty-state-mark">◌</span>
      <h2>浏览器预览</h2>
      <p>原生 SSH 能力只在 Tauri 桌面壳中可用，预览不会伪造远端服务数据。</p>
    </div>
  );
}

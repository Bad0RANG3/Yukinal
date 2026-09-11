import { useQuery } from "@tanstack/react-query";
import {
  IPC_COMMANDS,
  type LogLevel,
  type LogSource,
} from "@yukinal/shared";

import { errorMessage } from "../../lib/format.js";
import { callDesktop, isDesktopShell } from "../../lib/ipc.js";
import { useWorkspaceStore } from "../../stores/workspace-store.js";
import { EmptyPanel, ErrorPanel, LoadingPanel, PreviewEmpty } from "../../components/PanelStates.js";
import { Icon } from "../../components/Icon.js";
import { KeywordText } from "../../components/KeywordText.js";

const LEVEL_LABEL: Record<LogLevel, string> = {
  error: "错误",
  warning: "警告",
  info: "信息",
};

const SOURCE_LABEL: Record<LogSource, string> = {
  journalctl: "journalctl",
  syslog: "/var/log/syslog",
  messages: "/var/log/messages",
  unavailable: "未发现日志源",
};

export function LogsPane() {
  const selectedServerId = useWorkspaceStore((state) => state.selectedServerId);
  const shell = isDesktopShell();
  const logsQuery = useQuery({
    queryKey: ["serverLogs", selectedServerId],
    enabled: Boolean(selectedServerId) && shell,
    staleTime: 10_000,
    refetchInterval: 30_000,
    queryFn: async () => {
      if (!selectedServerId) return null;
      // `callDesktop` binds this command to the response schema registered in
      // `IPC_SCHEMAS`, so the command→schema pair has exactly one home.
      return callDesktop(IPC_COMMANDS.serverLogs, { serverId: selectedServerId });
    },
  });

  if (!shell) return <PreviewEmpty icon="logs" body="原生 SSH 能力只在 Tauri 桌面壳中可用，预览不会伪造远端日志数据。" />;

  if (!selectedServerId) {
    return <EmptyPanel icon="logs" title="选择一台服务器" body="从左侧列表选择目标环境，查看最近的远端日志。" />;
  }

  if (logsQuery.isLoading) {
    return <LoadingPanel title="正在读取最近日志" hint="SSH · 最多 120 行 · 预计几秒完成" />;
  }

  if (logsQuery.isError || !logsQuery.data) {
    return (
      <ErrorPanel
        showIcon
        title="无法读取远端日志"
        message={errorMessage(logsQuery.error)}
        onRetry={() => void logsQuery.refetch()}
      />
    );
  }

  const response = logsQuery.data;
  const errors = response.lines.filter((line) => line.level === "error").length;
  const warnings = response.lines.filter((line) => line.level === "warning").length;

  return (
    <div className="logs-page">
      <section className="logs-page-header">
        <div>
          <p className="eyebrow">远端日志</p>
          <h2>日志</h2>
          <p>保留原始行，最多读取最近 120 行，便于快速定位问题。</p>
        </div>
        <button type="button" className="button-secondary" onClick={() => void logsQuery.refetch()} disabled={logsQuery.isFetching} title="刷新日志" aria-label="刷新日志">
          <Icon name="refresh" size="sm" /> {logsQuery.isFetching ? "读取中" : "刷新"}
        </button>
      </section>

      <div className="log-summary">
        <span className={`log-source log-source-${response.source}`}>{SOURCE_LABEL[response.source]}</span>
        <span>{response.lines.length} 行{errors ? ` · ${errors} 个错误` : ""}{warnings ? ` · ${warnings} 个警告` : ""}</span>
      </div>

      {response.message ? <div className="log-notice">{response.message}</div> : null}

      {response.lines.length === 0 ? (
        <EmptyPanel extraClass="log-empty" icon="logs" title="没有可展示的日志" body={response.message ?? "日志源没有返回内容。"} />
      ) : (
        <ol className="log-list" aria-label="远端日志列表">
          {response.lines.map((line, index) => (
            <li className="log-row" key={`${index}:${line.text}`}>
              <span className="log-line-number" aria-hidden="true">{String(index + 1).padStart(3, "0")}</span>
              <span className={`log-level log-level-${line.level}`}>{LEVEL_LABEL[line.level]}</span>
              <code><KeywordText text={line.text} /></code>
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}

import type { ServerSnapshot } from "@yukinal/shared";
import { Icon } from "../../components/Icon.js";
import { formatBytes } from "../../lib/format.js";

export function ServerToolkitSections({ snapshot }: { snapshot: ServerSnapshot }) {
  const gpuFailure = snapshot.collectors?.find((sample) => ["gpu", "collector.gpu"].includes(sample.collectorId) && !sample.ok);
  const securityFailure = snapshot.collectors?.find((sample) => ["security", "collector.security"].includes(sample.collectorId) && !sample.ok);
  const gpu = gpuFailure ? undefined : snapshot.gpu;
  const security = securityFailure ? undefined : snapshot.security;
  const gpuError = gpuFailure ? gpuFailure.error ?? "检测失败" : undefined;
  const warnings = security?.checks.filter((check) => check.status === "warning").length ?? 0;
  const unknown = security?.checks.filter((check) => check.status === "unknown").length ?? 0;
  return <>
    <details className="overview-inspector-section" key={`gpu-${snapshot.serverId}`}>
      <summary><Icon name="chevronRight" size="xs" /><span>GPU</span><small>{gpu?.status === "available" ? gpu.devices.length > 1 ? `${gpu.devices.length} 张` : gpu.devices[0]?.usagePercent === undefined ? "利用率未知" : `${Math.round(gpu.devices[0].usagePercent)}%` : gpu?.message ?? (gpuError ? "检测失败" : "未报告")}</small></summary>
      <div className="overview-section-content">
        {gpu?.status === "available" ? gpu.devices.map((device) => <dl className="overview-properties" key={device.index}>
          <div><dt>设备 {device.index}</dt><dd>{device.name}</dd></div>
          <div><dt>利用率</dt><dd>{device.usagePercent === undefined ? "—" : `${Math.round(device.usagePercent)}%`}</dd></div>
          <div><dt>显存</dt><dd>{formatBytes(device.memoryUsedBytes)} / {formatBytes(device.memoryTotalBytes)}</dd></div>
          <div><dt>温度</dt><dd>{device.temperatureCelsius === undefined ? "—" : `${device.temperatureCelsius} °C`}</dd></div>
        </dl>) : <p className="muted-copy">{gpu?.message ?? gpuError ?? "尚未返回 GPU 检测结果。"}</p>}
        {gpu?.status !== "available" ? <p className="muted-copy">当前支持 NVIDIA 设备检测。</p> : null}
      </div>
    </details>
    <details className="overview-inspector-section" key={`security-${snapshot.serverId}`}>
      <summary><Icon name="chevronRight" size="xs" /><span>安全信息</span><small>{securityFailure ? "检测失败" : !security ? "未报告" : warnings ? `${warnings} 项提醒` : unknown ? `${unknown} 项未确认` : "已采集"}</small></summary>
      <div className="overview-section-content">
        {!security ? <p className="muted-copy">{securityFailure?.error ?? "尚未返回安全信息。"}</p> : <ul className="overview-toolkit-checks">{security.checks.map((check) => <li key={check.id} data-state={check.status}>
          <div><strong>{check.label}</strong><small>{check.status === "warning" ? "需留意" : check.status === "unknown" ? "未确认" : "已读取"}</small></div>
          <p>{check.detail}</p>
        </li>)}</ul>}
        <p className="muted-copy">本机配置；监听端口不代表公网可达，云防火墙与 SSH 条件规则另计。</p>
      </div>
    </details>
  </>;
}

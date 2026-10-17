import type { NetworkDetails, NetworkRoute, ServerSnapshot } from "@yukinal/shared";
import { Icon } from "../../components/Icon.js";
import { formatBytes } from "../../lib/format.js";
import { networkChecks } from "./network-model.js";

function Routes({ label, routes, error }: { label: string; routes?: NetworkRoute[]; error?: string }) {
  return <>
    <h3 className="overview-subsection-title">{label}</h3>
    {error ? <p className="network-read-error">{error}</p> : routes === undefined ? <p className="muted-copy">未采集</p> : !routes.length ? <p className="muted-copy">无路由</p> : <ul className="overview-property-list network-route-list">{routes.map((route, index) => <li key={index}>
      <strong>{route.destination === "default" ? "默认路由" : route.destination}{route.gateway ? ` → ${route.gateway}` : ""}</strong>
      <span>{[route.interface, route.type && route.type !== "unicast" ? route.type : null, route.metric === undefined ? null : `优先级 ${route.metric}`].filter(Boolean).join(" · ") || "未报告下一跳"}</span>
    </li>)}</ul>}
  </>;
}

export function ServerNetworkSection({ details, traffic, onRefresh, refreshing, connected }: {
  details?: NetworkDetails;
  traffic?: ServerSnapshot["network"];
  onRefresh: () => void;
  refreshing: boolean;
  connected: boolean;
}) {
  const checks = networkChecks(details);
  const warnings = checks.filter((check) => check.state === "warning");
  const unknown = checks.some((check) => check.state === "unknown");
  const interfaces = details?.errors?.interfaces ? undefined : details?.interfaces;
  const dns = details?.errors?.dnsServers ? undefined : details?.dnsServers;
  const status = !connected ? "缓存" : warnings.length ? warnings.map((check) => check.label).join("、") + "待查看" : unknown ? "信息不完整" : "已读取配置";
  return <details className="overview-inspector-section overview-network">
    <summary><Icon name="chevronRight" size="xs" /><span>网络与路由</span><small className={warnings.length ? "network-warning" : undefined}>{status}</small></summary>
    <div className="overview-section-content">
      <div className="network-check-heading"><span>{connected ? "只读配置检查" : "缓存配置"}</span><button type="button" className="icon-button" title="刷新网络与路由" aria-label="刷新网络与路由" disabled={refreshing || !connected} onClick={onRefresh}><Icon name="refresh" size="sm" /></button></div>
      <dl className="overview-properties network-checks">{checks.map((check) => <div key={check.label}><dt>{check.label}</dt><dd className={`network-check-${check.state}`} title={check.value}>{check.state === "ok" ? check.detail ?? check.value : check.value}</dd></div>)}</dl>
      <details className="network-full-config">
      <summary><Icon name="chevronRight" size="xs" />完整配置<small>{interfaces ? `${interfaces.length} 个网卡` : "网卡未采集"}</small></summary>
      <h3 className="overview-subsection-title">网卡与地址</h3>
      {details?.errors?.interfaces ? <p className="network-read-error">{details.errors.interfaces}</p> : interfaces === undefined ? <p className="muted-copy">未采集</p> : !interfaces.length ? <p className="muted-copy">无网卡</p> : <ul className="overview-property-list network-interface-list">{interfaces.map((item) => {
        const bytes = traffic?.find((sample) => sample.name === item.name);
        return <li key={item.name}><div><strong>{item.name}</strong><small>{item.state}</small></div><span>{item.addresses.join(" · ") || "无 IP 地址"}</span>{bytes ? <span>累计收 {formatBytes(bytes.rxBytes)} · 发 {formatBytes(bytes.txBytes)}</span> : null}</li>;
      })}</ul>}
      <Routes label="IPv4 路由 · 主路由表" routes={details?.ipv4Routes} error={details?.errors?.ipv4Routes} />
      <Routes label="IPv6 路由 · 主路由表" routes={details?.ipv6Routes} error={details?.errors?.ipv6Routes} />
      <h3 className="overview-subsection-title">DNS · resolv.conf</h3>
      {details?.errors?.dnsServers ? <p className="network-read-error">{details.errors.dnsServers}</p> : dns === undefined ? <p className="muted-copy">未采集</p> : <p className="network-dns">{dns.join(" · ") || "无 nameserver 配置"}</p>}
      {!interfaces && traffic?.length ? <><h3 className="overview-subsection-title">网络累计流量</h3><ul className="overview-property-list">{traffic.map((item) => <li key={item.name}><strong>{item.name}</strong><span>累计收 {formatBytes(item.rxBytes)} · 发 {formatBytes(item.txBytes)}</span></li>)}</ul></> : null}
      </details>
      <p className="network-scope">主路由表 · 可达性未探测</p>
    </div>
  </details>;
}

import type { NetworkDetails, NetworkRoute } from "@yukinal/shared";

export type NetworkCheck = { label: string; value: string; detail?: string; state: "ok" | "warning" | "unknown" };
const DEFAULTS = new Set(["default", "0.0.0.0/0", "::/0"]);
export function isDefaultRoute(route: NetworkRoute): boolean {
  return DEFAULTS.has(route.destination) && (!route.type || route.type === "unicast");
}
function isHostAddress(address: string): boolean {
  return !/^(127\.|0\.0\.0\.0\/|::[01]\/|fe[89ab][0-9a-f]:|169\.254\.)/i.test(address);
}
export function networkChecks(details?: NetworkDetails): NetworkCheck[] {
  const interfaces = details?.errors?.interfaces ? undefined : details?.interfaces;
  const addressed = interfaces?.filter((item) => item.name !== "lo" && item.state !== "DOWN" && item.addresses.some(isHostAddress));
  const routes4 = details?.errors?.ipv4Routes ? undefined : details?.ipv4Routes;
  const routes6 = details?.errors?.ipv6Routes ? undefined : details?.ipv6Routes;
  const defaults = [...(routes4 ?? []), ...(routes6 ?? [])].filter(isDefaultRoute);
  const routeUnknown = routes4 === undefined || routes6 === undefined;
  const dns = details?.errors?.dnsServers ? undefined : details?.dnsServers;
  const host = addressed?.[0];
  const address = host?.addresses.find(isHostAddress);
  const route = defaults[0];
  const extra = (count: number) => count > 1 ? ` +${count - 1}` : "";
  return [
    { label: "主机地址", detail: address && host ? `${address} · ${host.name}${extra(addressed?.length ?? 0)}` : undefined, value: addressed === undefined ? "未读取" : addressed.length ? `${addressed.length} 个网卡有地址` : "无可用地址", state: addressed === undefined ? "unknown" : addressed.length ? "ok" : "warning" },
    { label: "默认路由", detail: route ? [route.gateway ?? (route.interface ? "直连" : "未报告下一跳"), route.interface].filter(Boolean).join(" · ") + extra(defaults.length) : undefined, value: defaults.length ? `${defaults.length} 条${routeUnknown ? " · 部分未读取" : ""}` : routeUnknown ? "未完整读取" : "未配置", state: routeUnknown ? "unknown" : defaults.length ? "ok" : "warning" },
    { label: "DNS", detail: dns?.length ? dns[0] + extra(dns.length) : undefined, value: dns === undefined ? "未读取" : dns.length ? `${dns.length} 个解析器${dns.some((ip) => ip.startsWith("127.") || ip === "::1") ? " · 含本机代理" : ""}` : "未配置", state: dns === undefined ? "unknown" : dns.length ? "ok" : "warning" },
  ];
}

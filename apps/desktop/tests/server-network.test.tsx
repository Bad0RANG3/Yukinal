import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import { IPC_COMMANDS, IPC_SCHEMAS, type NetworkDetails } from "@yukinal/shared";
import { networkChecks } from "../src/features/overview/network-model.js";
import { ServerNetworkSection } from "../src/features/overview/ServerNetworkSection.js";
import { previewResponse } from "../src/lib/preview-fixtures.js";

const details: NetworkDetails = {
  interfaces: [{ name: "eth0", state: "UP", addresses: ["10.24.1.10/24"] }],
  ipv4Routes: [{ destination: "default", gateway: "10.24.1.1", interface: "eth0" }],
  ipv6Routes: [], dnsServers: ["127.0.0.53"],
};

test("route checks distinguish configured, absent, unknown and failed sections", () => {
  assert.ok(networkChecks(details).every((check) => check.state === "ok"));
  assert.equal(networkChecks({ ...details, ipv4Routes: [] })[1]?.value, "未配置");
  assert.ok(networkChecks().every((check) => check.state === "unknown"));
  const failed = networkChecks({ ...details, errors: { ipv4Routes: "permission denied" } });
  assert.equal(failed[1]?.state, "unknown");
  assert.ok(!failed[1]?.value.includes("1 条"));
  assert.equal(networkChecks({ ...details, ipv4Routes: [{ destination: "default", type: "blackhole" }] })[1]?.state, "warning");
});
test("loopback, link-local and down interfaces are not usable host addresses", () => {
  const interfaces = [
    { name: "lo", state: "UNKNOWN", addresses: ["127.0.0.1/8", "::1/128"] },
    { name: "eth0", state: "UP", addresses: ["feb0::1/64", "169.254.1.2/16"] },
    { name: "eth1", state: "DOWN", addresses: ["10.24.1.10/24"] },
  ];
  assert.equal(networkChecks({ ...details, interfaces })[0]?.state, "warning");
});
test("inspector labels configuration scope and cumulative bytes without inventing reachability", () => {
  const render = (connected: boolean) => renderToStaticMarkup(<ServerNetworkSection details={details} traffic={[{ name: "eth0", rxBytes: 100, txBytes: 200 }]} onRefresh={() => {}} refreshing={false} connected={connected} />);
  const html = render(true);
  for (const text of ["网络与路由", "10.24.1.1", "127.0.0.53", "累计收", "可达性未探测"]) assert.ok(html.includes(text), text);
  assert.ok(!html.includes("网络正常"));
  assert.match(render(false), /缓存配置/);
  assert.match(render(false), /disabled=""/);
});
test("network fixtures cover complete routes, missing defaults and unavailable route reads", () => {
  const snapshot = (serverId: string) => IPC_SCHEMAS[IPC_COMMANDS.serverSnapshot].response.parse(previewResponse(IPC_COMMANDS.serverSnapshot, { serverId })).snapshot!;
  assert.equal(networkChecks(snapshot("srv_previewapi").networkDetails)[1]?.state, "ok");
  assert.equal(networkChecks(snapshot("srv_previewdb").networkDetails)[1]?.state, "warning");
  assert.equal(networkChecks(snapshot("srv_previewinference").networkDetails)[1]?.state, "unknown");
});

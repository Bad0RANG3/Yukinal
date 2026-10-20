import assert from "node:assert/strict";
import test from "node:test";
import { ContainerInfoSchema, ServerSnapshotSchema } from "./collector.js";

const legacy = { id: "snap_test", serverId: "srv_test", collectedAt: "2026-10-17T00:00:00Z", health: "unknown", capabilities: {} };
test("empty legacy container state is unknown without discarding its actual status", () => {
  const container = { name: "web", image: "web:test", state: "", status: "Up 2 minutes", restartCount: 0 };
  assert.deepEqual(ContainerInfoSchema.parse(container), { ...container, state: "unknown" });
  assert.equal(ContainerInfoSchema.parse({ ...container, state: "running" }).state, "running");
  assert.equal(ContainerInfoSchema.safeParse({ ...container, state: null }).success, false);
});
test("network configuration extends snapshots without breaking legacy or partial results", () => {
  assert.doesNotThrow(() => ServerSnapshotSchema.parse(legacy));
  const snapshot = ServerSnapshotSchema.parse({ ...legacy, networkDetails: {
    interfaces: [{ name: "eth0", state: "UP", addresses: ["10.24.1.10/24"] }],
    ipv4Routes: [], errors: { ipv6Routes: "iproute2 unavailable" }, dnsServers: ["127.0.0.53", "fe80::53%eth0"],
  } });
  assert.deepEqual(snapshot.networkDetails?.ipv4Routes, []);
  assert.equal(snapshot.networkDetails?.ipv6Routes, undefined);
});
test("network details reject contract drift, negative route metrics and oversized collections", () => {
  for (const details of [
    { reachable: true },
    { interfaces: [{ name: "eth0", state: "UP", addresses: ["invalid-address"] }] },
    { dnsServers: ["resolver.invalid"] },
    { ipv4Routes: [{ destination: "default", metric: -1 }] },
    { interfaces: Array.from({ length: 257 }, () => ({ name: "eth0", state: "UP", addresses: [] })) },
    { errors: { other: "unsupported" } },
  ]) assert.equal(ServerSnapshotSchema.safeParse({ ...legacy, networkDetails: details }).success, false);
});

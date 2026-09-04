import assert from "node:assert/strict";
import test from "node:test";

import type { HostContextKind, Server, ServerSnapshot, Workspace } from "@yukinal/shared";

import { HostRpcClient } from "../transport/host-client.js";
import { createHostContextSource } from "./host-context-source.js";

const server: Server = {
  id: "srv_01abc",
  name: "Staging API",
  connection: { host: "api.internal", port: 22, username: "deploy" },
  capabilities: { linux: true, docker: true },
  status: "connected",
  metadata: { environment: "staging", region: "Singapore" },
  createdAt: "2026-01-01T00:00:00.000Z",
  updatedAt: "2026-01-01T00:00:00.000Z",
};

const snapshot: ServerSnapshot = {
  id: "snp_01",
  serverId: server.id,
  collectedAt: "2026-01-02T00:00:00.000Z",
  health: "healthy",
  capabilities: server.capabilities,
};

const workspace: Workspace = {
  id: "wsp_shop",
  name: "Shop",
  serverIds: [server.id],
  repositories: [],
  providerIds: [],
  defaultEnvironment: "staging",
};

test("host context source requests and validates server, snapshot, and workspace rows", async () => {
  const rows: Record<HostContextKind, unknown> = { server, snapshot, workspace };
  let client!: HostRpcClient;
  client = new HostRpcClient((raw) => {
    const request = JSON.parse(raw) as { id: number; params: { kind: HostContextKind } };
    client.handleIncoming({
      jsonrpc: "2.0",
      id: request.id,
      result: { status: "success", data: rows[request.params.kind] },
    });
  });
  const source = createHostContextSource(client);

  assert.deepEqual(await source.server(server.id), server);
  assert.deepEqual(await source.snapshot(server.id), snapshot);
  assert.deepEqual(await source.workspace(workspace.id), workspace);
});

test("a missing host context row becomes undefined, while a malformed row fails closed", async () => {
  let mode: "missing" | "malformed" = "missing";
  let client!: HostRpcClient;
  client = new HostRpcClient((raw) => {
    const request = JSON.parse(raw) as { id: number };
    client.handleIncoming({
      jsonrpc: "2.0",
      id: request.id,
      result:
        mode === "missing"
          ? { status: "not_found" }
          : { status: "success", data: { id: "not-a-server", name: "broken" } },
    });
  });
  const source = createHostContextSource(client);

  assert.equal(await source.server("srv_missing"), undefined);
  mode = "malformed";
  await assert.rejects(source.server("srv_broken"));
});

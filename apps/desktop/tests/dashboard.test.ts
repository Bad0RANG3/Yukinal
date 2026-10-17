import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import type { ServerSnapshot } from "@yukinal/shared";
import { dashboardResources } from "../src/features/overview/dashboard-model.js";
import { useWorkspaceStore } from "../src/stores/workspace-store.js";

afterEach(() => useWorkspaceStore.setState(useWorkspaceStore.getInitialState(), true));
const snapshot: ServerSnapshot = {
  id: "snap_test", serverId: "srv_a", collectedAt: "2026-10-16T08:00:00Z", health: "healthy", capabilities: {},
  cpu: { model: "CPU", cores: 4, usagePercent: 28, loadAverage: [1, 1, 1] },
  memory: { totalBytes: 100, usedBytes: 40, availableBytes: 60, usagePercent: 40 },
  disks: [{ device: "sda", mountPoint: "/", totalBytes: 100, usedBytes: 70, usagePercent: 70 }],
  docker: { available: true, containers: [{ name: "api", image: "api:1", state: "running", status: "Up", restartCount: 0 }] },
};
test("resource views cannot display another server's metrics or nested resources", () => {
  const resources = dashboardResources("srv_other", snapshot);
  assert.equal(resources.snapshot, undefined);
  assert.equal(resources.cpu, undefined);
  assert.equal(resources.containers, undefined);
});
test("failed collectors do not turn old metrics into current results", () => {
  const resources = dashboardResources("srv_a", { ...snapshot, collectors: [
    { collectorId: "collector.cpu", ok: false, collectedAt: snapshot.collectedAt },
    { collectorId: "docker", ok: false, collectedAt: snapshot.collectedAt },
  ] });
  assert.equal(resources.cpu, undefined);
  assert.equal(resources.containers, undefined);
  assert.equal(resources.memory, 40);
  assert.equal(resources.disk, 70);
});
test("missing resource data stays unknown rather than reporting zero", () => {
  const resources = dashboardResources("srv_a", { ...snapshot, disks: [], docker: { available: false, containers: [] } });
  assert.equal(resources.disk, undefined);
  assert.equal(resources.containers, undefined);
});
test("returning to Activity preserves the AI draft and selected target", () => {
  const workspace = useWorkspaceStore.getState();
  workspace.setAgentDraft("保留这段排查问题");
  workspace.selectServer("srv_a");
  workspace.setServerPage("files");
  assert.equal(useWorkspaceStore.getState().serverDetailOpen, true);
  workspace.setPrimary("activity");
  assert.equal(useWorkspaceStore.getState().primary, "activity");
  assert.equal(useWorkspaceStore.getState().serverDetailOpen, false);
  assert.equal(useWorkspaceStore.getState().selectedServerId, "srv_a");
  assert.equal(useWorkspaceStore.getState().agentDraft, "保留这段排查问题");
  workspace.selectServer("srv_a");
  assert.equal(useWorkspaceStore.getState().serverPage, "files");
});

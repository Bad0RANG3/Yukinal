import assert from "node:assert/strict";
import test from "node:test";
import { IPC_COMMANDS, IPC_SCHEMAS } from "@yukinal/shared";
import { previewMemoryResponse } from "../src/lib/preview-memory.js";

test("mock memory scopes use the real contract and cannot leak another server", () => {
  const list = (params: unknown) => IPC_SCHEMAS.agent_memory_list.response.parse(previewMemoryResponse(IPC_COMMANDS.agentMemoryList, IPC_SCHEMAS.agent_memory_list.params.parse(params))).memories;
  const globals = list({ scope: "global" });
  assert.ok(globals.length);
  assert.ok(globals.every((memory) => memory.scope === "global" && !memory.serverId));
  assert.ok(list({ scope: "server", serverId: "srv_previewdb" }).every((memory) => memory.serverId === "srv_previewdb"));
  assert.deepEqual(list({ scope: "server", serverId: "srv_absent" }), []);
  assert.deepEqual(list({ scope: "server", serverId: "srv_previewapi", query: "备份" }), []);
  const { memory } = IPC_SCHEMAS.agent_memory_save.response.parse(previewMemoryResponse(IPC_COMMANDS.agentMemorySave, { scope: "server", serverId: "srv_previewdb", title: "测试索引", content: "预览专用内容" }));
  assert.ok(list({ scope: "server", serverId: "srv_previewdb", query: "预览专用" }).some((item) => item.id === memory.id));
  assert.throws(() => previewMemoryResponse(IPC_COMMANDS.agentMemorySave, { ...memory, serverId: "srv_previewapi" }), /范围不匹配/);
  assert.equal(IPC_SCHEMAS.agent_memory_delete.response.parse(previewMemoryResponse(IPC_COMMANDS.agentMemoryDelete, { id: memory.id, scope: "server", serverId: "srv_previewapi" })).deleted, false);
  assert.equal(IPC_SCHEMAS.agent_memory_delete.response.parse(previewMemoryResponse(IPC_COMMANDS.agentMemoryDelete, { id: memory.id, scope: "server", serverId: "srv_previewdb" })).deleted, true);
});

test("mock memory honours expiry, version links and proposal provenance", () => {
  const list = (params: unknown) => IPC_SCHEMAS.agent_memory_list.response.parse(previewMemoryResponse(IPC_COMMANDS.agentMemoryList, IPC_SCHEMAS.agent_memory_list.params.parse(params))).memories;
  const future = new Date(Date.now() + 60 * 60 * 1000).toISOString();
  const past = new Date(Date.now() - 60 * 1000).toISOString();
  const saved = (input: unknown) => IPC_SCHEMAS.agent_memory_save.response.parse(previewMemoryResponse(IPC_COMMANDS.agentMemorySave, IPC_SCHEMAS.agent_memory_save.params.parse(input))).memory;

  const expiring = saved({ scope: "global", title: "过期预览", content: "预览过期内容", expiresAt: past });
  assert.equal(expiring.expiresAt, past);
  assert.ok(!list({ scope: "global" }).some((item) => item.id === expiring.id), "an expired memory must leave the listing");
  const lasting = saved({ scope: "global", title: "有效预览", content: "预览有效内容", expiresAt: future, supersedes: expiring.id, origin: "model" });
  assert.equal(lasting.origin, "model");
  assert.equal(lasting.supersedes, expiring.id);
  assert.ok(list({ scope: "global" }).some((item) => item.id === lasting.id));
  // The RPC surface refuses a naive instant before the mock ever stores it.
  assert.equal(IPC_SCHEMAS.agent_memory_save.params.safeParse({ scope: "global", title: "t", content: "c", expiresAt: "2030-01-01T00:00:00" }).success, false);

  // Superseded active memories leave the active listing once replaced by a live successor.
  const replaced = saved({ scope: "global", title: "旧版本", content: "旧内容" });
  assert.ok(list({ scope: "global" }).some((item) => item.id === replaced.id));
  const successor = saved({ scope: "global", title: "新版本", content: "新内容", supersedes: replaced.id });
  const visibleIds = list({ scope: "global" }).map((item) => item.id);
  assert.ok(visibleIds.includes(successor.id));
  assert.ok(!visibleIds.includes(replaced.id), "a superseded memory must not appear alongside its live successor");
});

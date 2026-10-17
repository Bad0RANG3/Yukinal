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

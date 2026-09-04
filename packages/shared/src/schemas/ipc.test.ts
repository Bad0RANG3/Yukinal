/**
 * Contract tests for the IPC command schemas (one side of the pair; the other lives
 * in `crates/core/src/ipc.rs` and asserts the *same* fixtures against serde output).
 *
 * The fixtures in `packages/shared/fixtures/ipc/` are the single canonical JSON each
 * side must accept/emit; a parse here is a promise that the Rust contract test keeps.
 */

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import type { IpcCommandName } from "../ipc/index.js";
import { FilesystemReadInputSchema, FilesystemWriteInputSchema } from "./file.js";
import {
  AgentStatusSchema,
  EMPTY_PAYLOAD,
  IpcServerIdSchema,
  IPC_SCHEMAS,
} from "./ipc.js";

const FIXTURE_DIR = new URL("../../fixtures/ipc/", import.meta.url);

function fixture(name: string): unknown {
  return JSON.parse(readFileSync(new URL(`${name}.json`, FIXTURE_DIR), "utf8")) as unknown;
}

test("every command in IpcCommandMap has a fixture and its response schema parses it", () => {
  const commands = Object.keys(IPC_SCHEMAS) as IpcCommandName[];
  assert.ok(commands.length >= 14, "the command map must not silently shrink");
  for (const command of commands) {
    const payload = fixture(command);
    const parsed = IPC_SCHEMAS[command].response.safeParse(payload);
    assert.equal(
      parsed.success,
      true,
      `${command} response schema must parse packages/shared/fixtures/ipc/${command}.json`,
    );
  }
});

test("the exited agent_status variant parses too", () => {
  const parsed = AgentStatusSchema.safeParse(fixture("agent_status_exited"));
  assert.equal(parsed.success, true);
});

test("responses are strict: a serde drift (extra field) must fail, not be stripped", () => {
  const drifted = { ...(fixture("agent_status") as Record<string, unknown>), toolCount: 1, os: "klingon" };
  assert.equal(AgentStatusSchema.safeParse(drifted).success, false);
});

test("empty payloads reject unknown keys", () => {
  assert.equal(EMPTY_PAYLOAD.safeParse({}).success, true);
  assert.equal(EMPTY_PAYLOAD.safeParse({ anything: 1 }).success, false);
});

test("server ids on the wire must be opaque srv_ ids", () => {
  for (const name of ["server_connect", "server_disconnect", "server_snapshot"] as const) {
    assert.equal(
      IPC_SCHEMAS[name].params.safeParse({ serverId: "api.example.com:22" }).success,
      false,
      `${name} must reject a host-derived id`,
    );
    assert.equal(IPC_SCHEMAS[name].params.safeParse({ serverId: "srv_01abc" }).success, true);
  }
  const open = IPC_SCHEMAS.terminal_open.params.safeParse({
    serverId: "api.example.com:22",
    cols: 120,
    rows: 30,
  });
  assert.equal(open.success, false, "terminal_open must reject a host-derived id");
  assert.equal(IpcServerIdSchema.safeParse("srv_01abc").success, true);
  assert.equal(IpcServerIdSchema.safeParse("production").success, false);
});

test("terminal param shapes match the contract", () => {
  const open = IPC_SCHEMAS.terminal_open.params.safeParse({
    serverId: "srv_01abc",
    cols: 120,
    rows: 30,
  });
  assert.equal(open.success, true);

  const write = IPC_SCHEMAS.terminal_write.params.safeParse({
    terminalSessionId: "t_01",
    data: "ls\r",
  });
  assert.equal(write.success, true);
});

test("add-server params are validated by the shared input schema", () => {
  const parsed = IPC_SCHEMAS.server_add.params.safeParse({
    name: "db",
    host: "10.0.0.5",
    username: "root",
    environment: "staging",
    authentication: { method: "password", password: "hunter2" },
  });
  assert.equal(parsed.success, true);
});

test("activity_list accepts an optional server filter and bounded limit", () => {
  const parsed = IPC_SCHEMAS.activity_list.params.safeParse({ serverId: "srv_01abc", limit: 25 });
  assert.equal(parsed.success, true);
  assert.equal(IPC_SCHEMAS.activity_list.params.safeParse({ serverId: "api.example.com:22" }).success, false);
  assert.equal(IPC_SCHEMAS.activity_list.params.safeParse({ limit: 0 }).success, false);
  assert.equal(IPC_SCHEMAS.activity_list.params.safeParse({ limit: 101 }).success, false);
});

test("tool_execution_list accepts trace/server filters and bounded limit", () => {
  assert.equal(
    IPC_SCHEMAS.tool_execution_list.params.safeParse({ traceId: "trc_1", limit: 25 }).success,
    true,
  );
  assert.equal(
    IPC_SCHEMAS.tool_execution_list.params.safeParse({ serverId: "srv_01abc", limit: 25 }).success,
    true,
  );
  assert.equal(
    IPC_SCHEMAS.tool_execution_list.params.safeParse({ serverId: "api.example.com:22" }).success,
    false,
  );
  assert.equal(IPC_SCHEMAS.tool_execution_list.params.safeParse({ limit: 0 }).success, false);
  assert.equal(IPC_SCHEMAS.tool_execution_list.params.safeParse({ limit: 101 }).success, false);
});

test("filesystem tool schemas bound paths, reads and writes", () => {
  assert.equal(FilesystemReadInputSchema.safeParse({ path: "/etc/app.env", maxBytes: 4096 }).success, true);
  assert.equal(FilesystemReadInputSchema.safeParse({ path: "relative/path" }).success, false);
  assert.equal(FilesystemReadInputSchema.safeParse({ path: "/tmp/file", maxBytes: 0 }).success, false);
  assert.equal(FilesystemWriteInputSchema.safeParse({ path: "/etc/app.env", content: "PORT=8080" }).success, true);
  assert.equal(FilesystemWriteInputSchema.safeParse({ path: "/etc/app.env", content: "x", extra: true }).success, false);
});

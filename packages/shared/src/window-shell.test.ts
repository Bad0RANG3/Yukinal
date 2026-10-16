import assert from "node:assert/strict";
import { test } from "node:test";
import { WindowShellContextSchema } from "./window-shell.js";

const context = { section: "servers", serverName: null, serverStatus: null, agentBusy: false };

test("window context covers workspace sections and server lifecycle states", () => {
  for (const section of ["servers", "projects", "tasks", "activity", "settings"]) {
    assert.equal(WindowShellContextSchema.safeParse({ ...context, section }).success, true);
  }
  for (const serverStatus of ["connecting", "connected", "disconnected", "error"]) {
    assert.equal(WindowShellContextSchema.safeParse({ ...context, serverName: "测试服务器", serverStatus, agentBusy: true }).success, true);
  }
});

test("window context excludes credentials, commands and approval state", () => {
  for (const extra of [{ apiKey: "test-only" }, { command: "server_connect" }, { approved: true }]) {
    assert.equal(WindowShellContextSchema.safeParse({ ...context, ...extra }).success, false);
  }
});

test("window context bounds Unicode names and rejects malformed payloads", () => {
  assert.equal(WindowShellContextSchema.safeParse({ ...context, serverName: "🛰".repeat(80) }).success, true);
  for (const serverName of ["🛰".repeat(80) + "a", "a".repeat(161), "a\nb", "a\0b", "a\x7fb", "\ud800"]) {
    assert.equal(WindowShellContextSchema.safeParse({ ...context, serverName }).success, false);
  }
  for (const value of [null, [], {}, { ...context, section: "terminal" }, { ...context, serverStatus: "unknown" }, { ...context, agentBusy: "true" }]) {
    assert.equal(WindowShellContextSchema.safeParse(value).success, false);
  }
});

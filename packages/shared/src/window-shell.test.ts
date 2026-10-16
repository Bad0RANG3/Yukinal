import assert from "node:assert/strict";
import { test } from "node:test";
import { WindowShellContextSchema, WindowShellSidebarSchema, WindowShellActionSchema } from "./window-shell.js";

const context = { section: "servers", serverName: null, serverStatus: null, agentBusy: false };

test("window context covers workspace sections and server lifecycle states", () => {
  for (const section of ["servers", "projects", "tasks", "activity", "settings"]) {
    assert.equal(WindowShellContextSchema.safeParse({ ...context, section }).success, true);
  }
  for (const serverStatus of ["connecting", "connected", "disconnected", "error"]) {
    assert.equal(WindowShellContextSchema.safeParse({ ...context, serverName: "测试服务器", serverStatus, agentBusy: true }).success, true);
  }
});

const server = { id: "srv_preview1", name: "测试", detail: "test.invalid:22", status: "disconnected" };
const sidebar = { selectedServerId: server.id, servers: [server], loading: false, failed: false, blocked: false };

test("native sidebar bounds text and identities without carrying execution parameters", () => {
  assert.equal(WindowShellSidebarSchema.safeParse(sidebar).success, true);
  for (const invalid of [
    { ...sidebar, servers: [server, server] },
    { ...sidebar, selectedServerId: "missing" },
    { ...sidebar, servers: [{ ...server, name: "🛰".repeat(81) }] },
    { ...sidebar, servers: [{ ...server, detail: "a\nb" }] },
    { ...sidebar, servers: [{ ...server, detail: "a".repeat(257) }] },
    { ...sidebar, servers: [{ ...server, status: "unknown" }] },
    { ...sidebar, servers: [{ ...server, apiKey: "test-only" }] },
    { ...sidebar, blocked: "true" }, { ...sidebar, approved: true },
  ]) assert.equal(WindowShellSidebarSchema.safeParse(invalid).success, false);
});

test("native actions are exact UI intents with validated server IDs", () => {
  for (const type of ["select-server", "edit-server", "connect-server", "disconnect-server", "delete-server"])
    assert.equal(WindowShellActionSchema.safeParse({ type, serverId: server.id }).success, true);
  for (const value of [{ type: "navigate", section: "settings" }, { type: "add-server" }, { type: "refresh-servers" }])
    assert.equal(WindowShellActionSchema.safeParse(value).success, true);
  for (const value of [{ type: "execute", command: "server_connect" }, { type: "connect-server", serverId: "bad" },
    { type: "navigate", section: "unknown" }, { type: "add-server", approved: true },
    { type: "connect-server", serverId: server.id, host: "test.invalid" }])
    assert.equal(WindowShellActionSchema.safeParse(value).success, false);
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

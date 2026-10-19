import assert from "node:assert/strict";
import test from "node:test";
import { registerHooks } from "node:module";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderToStaticMarkup } from "react-dom/server";
import type { ReactNode } from "react";

import { InvestigationsPane } from "../src/features/investigations/InvestigationsPane.js";
import { installHost, removeHost } from "./host-mock.js";

// Node renders the real component tree; styles remain the browser build's job.
const cssLoader = registerHooks({ load: (url, context, nextLoad) => url.endsWith(".css")
  ? { format: "module", source: "", shortCircuit: true }
  : nextLoad(url, context) });
const { TeamBoardPane } = await import("../src/features/agent-nist/TeamBoardPane.js");
cssLoader.deregister();

Object.defineProperty(globalThis, "window", { value: {}, configurable: true });

const server = {
  id: "srv_offline",
  name: "Offline staging",
  connection: { host: "staging.example.test", port: 2222, username: "test" },
  capabilities: {},
  status: "disconnected",
  metadata: { environment: "staging" },
  createdAt: "2026-01-01T00:00:00Z",
  updatedAt: "2026-01-01T00:00:00Z",
};

function renderOffline(content: ReactNode) {
  const calls: string[] = [];
  installHost({ invoke: async (command) => {
    calls.push(command);
    throw new Error(`Unexpected offline invocation: ${command}`);
  } });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity, staleTime: Infinity } } });
  client.setQueryData(["servers"], [server]);
  client.setQueryData(["team-board-tasks"], []);
  client.setQueryData(["investigationTasks"], { tasks: [] });
  client.setQueryData(["investigationSchedules"], { schedules: [] });
  try {
    return { markup: renderToStaticMarkup(<QueryClientProvider client={client}>{content}</QueryClientProvider>), calls };
  } finally {
    client.clear();
    removeHost();
  }
}

test("the default empty board exposes a keyboard button for goal tasks while keeping its lanes", () => {
  const { markup, calls } = renderOffline(<TeamBoardPane serverId={null} />);
  assert.match(markup, /<h2>看板<\/h2>/u);
  assert.match(markup, /<button type="button" class="button-secondary">目标任务<\/button>/u);
  assert.match(markup, /aria-label="看板服务器范围"/u);
  assert.match(markup, /class="team-board-columns"/u);
  assert.doesNotMatch(markup, /id="investigation-objective"/u);
  assert.deepEqual(calls, []);
});

test("the reused goal form offers configured targets and keeps automatic execution opt-in", () => {
  const { markup, calls } = renderOffline(<InvestigationsPane />);
  assert.match(markup, /<label[^>]+for="investigation-server">目标服务器 \/ 运行范围<\/label>/u);
  assert.match(markup, /<option value="srv_offline">Offline staging · staging\.example\.test:2222 · staging<\/option>/u);
  assert.match(markup, /id="investigation-objective"/u);
  assert.match(markup, /变更仍受审批/u);
  const delegation = markup.match(/<label class="investigation-task-delegation">[\s\S]*?<\/label>/u)?.[0];
  assert.ok(delegation);
  assert.doesNotMatch(delegation, /checked=""/u);
  const start = markup.match(/<button[^>]+class="button-primary"[^>]*>/u)?.[0];
  assert.ok(start);
  assert.match(start, /disabled=""/u);
  assert.deepEqual(calls, []);
});
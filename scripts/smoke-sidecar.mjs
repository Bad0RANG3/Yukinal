#!/usr/bin/env node
/**
 * End-to-end smoke test of the sidecar over its real transport (ADR 0001 / ADR 0006).
 *
 * Spawns `apps/agent` the same way Rust does, speaking NDJSON on stdin/stdout,
 * and asserts the protocol behaves as contracted -- including the honest
 * NOT_IMPLEMENTED answer for the agent loop, and survival of a malformed frame.
 */

import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import { once } from "node:events";
import { fileURLToPath } from "node:url";

const require = createRequire(fileURLToPath(new URL("../apps/agent/package.json", import.meta.url)));
const tsxCli = require.resolve("tsx/cli");
const entry = fileURLToPath(new URL("../apps/agent/src/index.ts", import.meta.url));

const child = spawn(process.execPath, [tsxCli, entry], {
  env: { ...process.env, YUKINAL_LOG_LEVEL: "warn", YUKINAL_DATA_DIR: "/tmp/yukinal-smoke" },
  stdio: ["pipe", "pipe", "pipe"],
});

child.stderr.setEncoding("utf8");
child.stderr.on("data", (chunk) => process.stderr.write(`  agent$ ${chunk}`));

let buffer = "";
const queue = [];
const waiters = [];
child.stdout.setEncoding("utf8");
child.stdout.on("data", (chunk) => {
  buffer += chunk;
  let index = buffer.indexOf("\n");
  while (index !== -1) {
    const line = buffer.slice(0, index);
    buffer = buffer.slice(index + 1);
    if (line.trim() !== "") {
      const frame = JSON.parse(line);
      // 只把「响应帧」（带 id）交给 ask；agent.* 通知不进匹配队列，
      // 否则后台 run 的事件会把后来的断言喂错帧。
      if (typeof frame.id === "number") {
        const waiter = waiters.shift();
        if (waiter) waiter(frame);
        else queue.push(frame);
      }
    }
    index = buffer.indexOf("\n");
  }
});

const send = (raw) => child.stdin.write(typeof raw === "string" ? raw : `${JSON.stringify(raw)}\n`);
const next = () => queue.shift() ?? new Promise((resolve) => waiters.push(resolve));

async function ask(frame, check) {
  send(frame);
  const reply = await next();
  check(reply);
  console.log(`✓ ${frame.method}`);
}

const assert = (condition, message) => {
  if (!condition) {
    console.error(`✗ ${message}`);
    process.exitCode = 1;
    throw new Error(message);
  }
};

await ask(
  { jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "1.0", clientVersion: "smoke", dataDir: "/tmp/yukinal-smoke" } },
  (frame) => assert(frame.result?.capabilities?.cancellation === true, `initialize failed: ${JSON.stringify(frame)}`),
);

await ask({ jsonrpc: "2.0", id: 2, method: "system.ping", params: { echo: "yukinal" } }, (frame) =>
  assert(frame.result?.pong === "yukinal" && typeof frame.result.agentPid === "number", `ping: ${JSON.stringify(frame)}`),
);

await ask({ jsonrpc: "2.0", id: 3, method: "tools.list", params: {} }, (frame) =>
  assert(
    frame.result?.tools?.some((tool) => tool.name === "system.echo" && tool.timeoutMs > 0),
    `tools.list: ${JSON.stringify(frame)}`,
  ),
);

await ask({ jsonrpc: "2.0", id: 4, method: "system.describe", params: {} }, (frame) =>
  assert(
    frame.result?.implemented?.["agent.run.start"] === true &&
      frame.result?.implemented?.["agent.run.stop"] === true &&
      frame.result?.implemented?.["agent.approval.respond"] === true &&
      frame.result.toolNameCollisions?.length === 0,
    `describe: ${JSON.stringify(frame)}`,
  ),
);

await ask({ jsonrpc: "2.0", id: 5, method: "agent.run.start", params: { runId: "r" } }, (frame) =>
  assert(frame.error?.code === -32602, `run.start should be INVALID_PARAMS for an incomplete request: ${JSON.stringify(frame)}`),
);

await ask(
  {
    jsonrpc: "2.0",
    id: 6,
    method: "agent.run.start",
    params: {
      runId: "smoke_run",
      sessionId: "ses",
      prompt: "hi",
      providerConfig: { kind: "openai-compatible", baseUrl: "http://127.0.0.1:1", model: "m" },
    },
  },
  // 真实 run（无可达端点）会在后台失败并走事件；这里只断言分发不炸 + 返回 runId。
  (frame) => {
    assert(!frame.error, `run.start should not error: ${JSON.stringify(frame)}`);
    assert(frame.result?.runId === "smoke_run", `run.start should echo runId: ${JSON.stringify(frame)}`);
  },
);

// A malformed frame must be dropped, not fatal.
send("{ this is not json\n");
await ask({ jsonrpc: "2.0", id: 7, method: "system.ping", params: {} }, (frame) =>
  assert(frame.result?.pong === "pong", `stream must survive a malformed frame: ${JSON.stringify(frame)}`),
);

child.stdin.end();
const [code] = await Promise.race([once(child, "exit"), new Promise((resolve) => setTimeout(() => resolve([-1]), 5_000))]);
assert(code === 0, `sidecar must exit when its parent closes stdin, got ${code}`);
console.log("✓ clean shutdown on stdin close");

console.log("sidecar smoke: green");

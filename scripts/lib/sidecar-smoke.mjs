/**
 * The sidecar protocol smoke, as a reusable routine (ADR 0001 / ADR 0006).
 *
 * Spawns a sidecar entry exactly the way Rust does -- `node <entry>`, speaking NDJSON
 * on stdin/stdout -- and asserts the protocol behaves as contracted, including the real
 * agent admission path and survival of a malformed frame.
 *
 * It lives here, not in a single `smoke-*.mjs`, because two different questions need the
 * same assertions: "does the built bundle speak the protocol?" (`smoke-sidecar.mjs` on
 * `apps/agent/dist/index.js`) and "does that same file still work when it is the only file
 * around, i.e. as `<resource_dir>/agent/index.js` in an installed app?"
 * (`smoke-packaged-agent.mjs`). Two copies of these assertions would drift, and the copy
 * that drifts is the one nobody runs.
 */

import { spawn } from "node:child_process";
import { once } from "node:events";

const assert = (condition, message) => {
  if (!condition) {
    console.error(`✗ ${message}`);
    process.exitCode = 1;
    throw new Error(message);
  }
};

/**
 * @param {string} entry absolute path to a runnable sidecar entry file
 * @param {{ env?: Record<string, string>, label?: string }} [options]
 */
export async function runSidecarSmoke(entry, options = {}) {
  const label = options.label ?? entry;
  console.log(`── sidecar smoke: node ${label}`);

  const child = spawn(process.execPath, [entry], {
    env: { ...process.env, YUKINAL_LOG_LEVEL: "warn", YUKINAL_DATA_DIR: "/tmp/yukinal-smoke", ...options.env },
    stdio: ["pipe", "pipe", "pipe"],
  });

  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => process.stderr.write(`  agent$ ${chunk}`));

  let buffer = "";
  const queue = [];
  const waiters = [];
  // The agent also talks *to* the host on this same stream (`host.mcp.catalog`, ADR 0014),
  // and its requests carry ids from its own counter — the same numbers we use. Direction,
  // not the id, is what tells the two apart: a frame with a `method` is a request, a frame
  // with only an `id` is an answer. Treating the former as an answer is what this harness
  // did until MCP landed, and it consumed the agent's catalog request as the reply to
  // `initialize`.
  const hostMethodsSeen = new Set();
  const HOST_HANDLERS = {
    // "No MCP servers are configured" is the ordinary host: an empty catalog, not an error.
    "host.mcp.catalog": () => ({ servers: [], failures: [] }),
  };
  child.stdout.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    buffer += chunk;
    let index = buffer.indexOf("\n");
    while (index !== -1) {
      const line = buffer.slice(0, index);
      buffer = buffer.slice(index + 1);
      if (line.trim() !== "") {
        const frame = JSON.parse(line);
        if (typeof frame.method === "string") {
          // Three frame kinds arrive here, and only one of them gets an answer:
          //   - a request from the agent (method + id) → answer it like the Rust host would;
          //   - an *upward notification* (method, no id) — `agent.stream` carries
          //     `AgentStreamEvent` (ADR 0006). Replying to one would put a frame on the wire
          //     that neither side expects, so it is recorded and dropped;
          //   - everything else is a reply, handled below.
          hostMethodsSeen.add(frame.method);
          if (typeof frame.id === "number") {
            const handler = HOST_HANDLERS[frame.method];
            // An unknown host method gets the answer a real Rust host gives. Being silent
            // would hang the agent, and inventing a result would hide a real gap.
            send(
              handler
                ? { jsonrpc: "2.0", id: frame.id, result: handler(frame.params) }
                : {
                    jsonrpc: "2.0",
                    id: frame.id,
                    error: { code: -32601, message: `this smoke host does not implement ${frame.method}` },
                  },
            );
            console.log(`✓ host request ${frame.method}`);
          }
        } else if (typeof frame.id === "number") {
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

  // Pinning the MCP wiring from the outside, where it cannot be faked: the agent must ask
  // the host for a catalog. Drop `loadMcpCatalog` and this smoke fails, which is the whole
  // reason it is asserted here and not only in a unit test with a fake client.
  assert(
    hostMethodsSeen.has("host.mcp.catalog"),
    "the agent never asked the host for an MCP catalog (ADR 0014)",
  );

  console.log("sidecar smoke: green");
}

/**
 * Sidecar entry point (ADR 0001). Nothing but wiring: the runtime lives in
 * `runtime/create-runtime.ts`, the protocol lives in `rpc/` and `transport/`.
 */

import { AGENT_VERSION, createLogger, readConfig } from "./config.js";
import { loadCatalogFromHost } from "./mcp/catalog.js";
import { KNOWN_POLICY_IDS } from "./permissions/policy-registry.js";
import { createRuntime, type Runtime } from "./runtime/create-runtime.js";
import { HostRpcClient } from "./transport/host-client.js";
import { startStdioRpc, type StdioServer } from "./transport/stdio.js";

let server: StdioServer | undefined;

export function main(): void {
  const config = readConfig();
  const log = createLogger({ level: config.logLevel, scope: "agent" });
  const hostToolClient = new HostRpcClient((frame) => process.stdout.write(frame));
  const runtime = createRuntime({ log, hostToolClient, maxRunMs: config.maxRunMs });

  server = startStdioRpc({
    router: runtime.router,
    log: log.child("stdio"),
    hostToolClient,
    // No parent stdin means the desktop is gone: exit instead of orphaning.
    onParentGone: () => shutdown("stdin-closed"),
  });

  // MCP tools are discovered **after** the RPC server is up, never before.
  //
  // Not a style choice: the host only republishes sidecar requests after it has finished the
  // initialize handshake (`crates/core/src/supervisor.rs` spawns the event pump after
  // `sidecar::handshake`), so a catalog request sent before `startStdioRpc` would wait for a
  // host that is still waiting for us, and the launch would time out
  // (`YUKINAL_AGENT_TIMEOUT_SECS`, 10s by default). The cost of getting this wrong is a
  // sidecar that never starts.
  //
  // The consequence is that `initialize` answers `capabilities.mcp` before the catalog exists.
  // That is answered truthfully by asking the registry at that moment (see the note in the
  // report / ADR 0014): the flag is a *fact about this session so far*, and
  // `agent.list_tools` / `system.describe.toolCount` are the live view.
  void loadMcpCatalog(runtime, hostToolClient, log);

  log.info("ready", {
    version: AGENT_VERSION,
    tools: runtime.declarations.map((declaration) => declaration.name),
    policies: KNOWN_POLICY_IDS,
    dataDir: config.dataDir === "" ? "(unset)" : config.dataDir,
    maxRunMs: config.maxRunMs,
  });
}

/**
 * Fetch the MCP catalog and register it. Never throws, never retries.
 *
 * Failures are logged and dropped: a dead MCP server must not keep the agent from answering,
 * and `capabilities.mcp` reports what actually happened rather than what was hoped for.
 */
async function loadMcpCatalog(
  runtime: Runtime,
  hostToolClient: HostRpcClient,
  log: ReturnType<typeof createLogger>,
): Promise<void> {
  await loadCatalogFromHost({
    registry: runtime.registry,
    // `executeOnHost` is the same host channel every other host-backed tool uses: MCP calls
    // are not a second execution path (ADR 0014).
    executeOnHost: (request, signal) => hostToolClient.execute(request, signal),
    fetchCatalog: (signal) => hostToolClient.fetchMcpCatalog(signal),
    log: (message, meta) => {
      log.info(message, meta);
    },
  });
}

export function shutdown(reason: string): void {
  server?.close();
  server = undefined;
  process.stderr.write(`agent sidecar shutting down (${reason})\n`);
  // 在途运行不会被取消，这里以前写的是一句已过期的待办（"In-flight cancellation
  // handling lands with the agent loop"）—— 那个 loop 早就落地了，而且**确实**有取消：
  // `AgentLoop.stop(runId)` 会 abort 该次运行的在途请求与待批等待，经 `agent.run.stop`
  // 暴露（`rpc/router.ts:120-123`）。
  //
  // 但这条路径够不着它，两点都是结构性的而非疏忽：`runtime` 是 `main()` 的局部变量，
  // 而 loop 没有公开任何「枚举在途 runId」的手段（`#tokensByRun` 是私有字段，
  // 唯一的 getter `pendingApprovals` 给的是 approval id，不是 runId）。所以要在这里
  // 取消，得先给 loop 加一个 stop-all 并把它接到模块级 —— 那是新增能力，不是重构。
  //
  // 两个调用方都不需要它：`stdin-closed` 意味着桌面已经不在了，没人会收到
  // `agent.cancelled`；而 SIGTERM 那条路实际走不到 —— 监督进程用的是
  // `child.start_kill()`（`sidecar/mod.rs:228`，即 SIGKILL），本函数根本不会被调用，
  // 这里只是手工起进程时按 Ctrl-C 的兜底。
  //
  // 什么时候这个判断会失效：如果某次运行的副作用需要「优雅收尾」（比如半途回滚），
  // 或者监督进程改成先发礼貌信号再杀，那么「直接放弃在途运行」就不再是可接受的了。
  //
  // 50ms 是上限而不是延时：`.unref()` 让这个定时器不占住事件循环，所以进程在其余
  // 工作排空后就会退出，这里只是给 stdout/stderr 留出冲刷的窗口。
  setTimeout(() => {
    process.exit(0);
  }, 50).unref();
}

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.on(signal, () => {
    shutdown(signal);
  });
}

main();

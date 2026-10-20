import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import vm from "node:vm";

// Exercise the actual shipped bootstrap and C++ event script, including their
// serialization boundary. A copied JS implementation would miss wiring drift.
const nativeSource = readFileSync(new URL("../../../native/desktop_host/src/main.cpp", import.meta.url), "utf8");
const bootstrapMatch = nativeSource.match(/constexpr const char\* kHostBridge = R"JS\(([\s\S]*?)\)JS";/u);
assert.ok(bootstrapMatch, "native host bootstrap must remain testable");
const bootstrap = bootstrapMatch[1]!;
const publishMatch = nativeSource.match(/const auto script =\s*("(?:\\.|[^"\\])*")\s*\+\s*nlohmann::json\(envelope\)\.dump\(\)\s*\+\s*("(?:\\.|[^"\\])*")\s*;/u);
assert.ok(publishMatch, "native publish serialization must remain testable");
const publishPrefix = JSON.parse(publishMatch[1]!) as string;
const publishSuffix = JSON.parse(publishMatch[2]!) as string;

type HostBridge = {
  listen(event: string, handler: (payload: unknown) => void): Promise<() => void>;
  invoke(command: string, params?: unknown): Promise<unknown>;
};
function makeWindow(invoke?: (command: string, params: unknown) => Promise<unknown>) {
  const calls: Array<{ command: string; params: unknown }> = [];
  const errors: unknown[][] = [];
  let token = 0;
  const window: Record<string, unknown> = {};
  window.top = window;
  window.yukinalNativeInvoke = async (command: string, params: unknown) => {
    calls.push({ command, params });
    return invoke ? invoke(command, params) : { subscribed: true };
  };
  const context = vm.createContext({ window, crypto: { randomUUID: () => `token-${++token}` }, console: { error: (...args: unknown[]) => errors.push(args) } });
  vm.runInContext(bootstrap, context);
  return { context, window, calls, errors, bridge: window.__YUKINAL_HOST__ as HostBridge };
}
function publish(context: vm.Context, event: string, payload: unknown) {
  const envelope = JSON.stringify({ event, payload });
  // nlohmann::json(envelope).dump() quotes the JSON document as a JS string.
  vm.runInContext(publishPrefix + JSON.stringify(envelope) + publishSuffix, context);
}

test("native event envelopes reach registered handlers without evaluating payload text", async () => {
  const host = makeWindow();
  const received: unknown[] = [];
  await host.bridge.listen("terminal.data", (payload) => received.push(payload));
  const payload = { terminalSessionId: "session-a", data: "\r\nprompt \"quoted\" \\ newline\n</script> ');window.injected=true;// \u2028\u2029" };
  publish(host.context, "terminal.data", payload);
  assert.equal(received.length, 1);
  assert.equal(JSON.stringify(received[0]), JSON.stringify(payload));
  assert.equal(host.window.injected, undefined);
  publish(host.context, "agent.text", { textDelta: "unregistered" });
  assert.equal(received.length, 1);
});

test("standalone window subscriptions stay independent and unsubscribe once", async () => {
  const main = makeWindow(), child = makeWindow();
  let mainCalls = 0, childCalls = 0;
  const stopMain = await main.bridge.listen("terminal.data", () => mainCalls++);
  await child.bridge.listen("terminal.data", () => childCalls++);
  publish(main.context, "terminal.data", { data: "first" });
  publish(child.context, "terminal.data", { data: "first" });
  assert.deepEqual([mainCalls, childCalls], [1, 1]);
  stopMain(); stopMain();
  publish(main.context, "terminal.data", { data: "second" });
  publish(child.context, "terminal.data", { data: "second" });
  assert.deepEqual([mainCalls, childCalls], [1, 2]);
  assert.equal(main.calls.filter((call) => call.command === "__events.unsubscribe").length, 1);
});

test("one failed handler does not swallow other subscribers", async () => {
  const host = makeWindow();
  let calls = 0;
  await host.bridge.listen("agent.text", () => { throw new Error("handler failed"); });
  await host.bridge.listen("agent.text", () => calls++);
  publish(host.context, "agent.text", { textDelta: "reply" });
  assert.equal(calls, 1);
  assert.equal(host.errors.length, 1);
});

test("a native subscription failure removes its local handler", async () => {
  const host = makeWindow(async () => { throw new Error("subscription failed"); });
  let calls = 0;
  await assert.rejects(host.bridge.listen("terminal.data", () => calls++), /subscription failed/u);
  publish(host.context, "terminal.data", { data: "ignored" });
  assert.equal(calls, 0);
});

test("subframes receive no high-level native host bridge", () => {
  const window = { top: {}, yukinalNativeInvoke: () => assert.fail("a subframe must not initialize subscriptions") };
  const context = vm.createContext({ window });
  vm.runInContext(bootstrap, context);
  assert.equal("__YUKINAL_HOST__" in window, false);
});

test("structured native pin errors reject instead of becoming a success state", async () => {
  const host = makeWindow(async () => { throw { code: "WINDOW_ERROR", message: "Could not change window pin state" }; });
  await assert.rejects(host.bridge.invoke("__shell.pin", { pinned: true }), /Could not change window pin state \(WINDOW_ERROR\)/u);
});

test("native shutdown skips queued RPC/events while destruction drains without its mutex", (t) => {
  // Compile the shipped gates and retirement helpers with a tiny WebView stub.
  // The stub deliberately drains queued work during destruction, as Win32 does.
  const cachePath = new URL("../../../native/build-cpp-host/CMakeCache.txt", import.meta.url);
  const cache = existsSync(cachePath) ? readFileSync(cachePath, "utf8") : "";
  const compiler = process.env.CXX || cache.match(/^CMAKE_CXX_COMPILER:[^=]+=(.+)$/mu)?.[1]?.trim() || "c++";
  const probe = spawnSync(compiler, ["--version"], { encoding: "utf8" });
  if (probe.error || probe.status !== 0) {
    t.skip("A C++ compiler is needed for the extracted native lifecycle regression");
    return;
  }
  const lifetime = nativeSource.match(/struct WindowEventLifetime \{[\s\S]*?\n\};/u)?.[0];
  const pendingEvent = nativeSource.match(/    struct PendingEvent \{([\s\S]*?)\n    \};/u)?.[0];
  const queuedEvent = nativeSource.match(/\[\]\(webview_t current, void\* value\) \{([\s\S]*?)\n        \},/u)?.[1];
  // Compile the real entry gate without depending on the dispatcher body or its local variables.
  const rpcGuard = nativeSource.match(/void rpc_handler\(const char\* id, const char\* request, void\* context_pointer\) \{[\s\S]*?(auto& context = \*static_cast<BridgeContext\*>\(context_pointer\);\s*if \([^\n]+\) return;)/u)?.[1];
  const active = nativeSource.match(/  bool active\(\) const \{[^\n]+\}/u)?.[0];
  const destroy = nativeSource.match(/void destroy_bridge_window\(BridgeContext& context\) \{[\s\S]*?\n\}/u)?.[0];
  const retire = nativeSource.match(/std::vector<std::unique_ptr<BridgeContext>> retire_auxiliary_windows\([\s\S]*?\n\}/u)?.[0];
  assert.ok(lifetime && pendingEvent && queuedEvent && rpcGuard && active && destroy && retire,
    "native lifecycle must expose its actual queued callback guards and retirement helpers");
  const harness = `
#include <atomic>
#include <cassert>
#include <deque>
#include <functional>
#include <memory>
#include <mutex>
#include <string>
#include <vector>
${lifetime}
struct BridgeContext;
struct Window { BridgeContext* owner; };
using webview_t = Window*;
struct Router { void set_event_sink(std::nullptr_t) {} };
struct Replies { bool closed = false; void close() { closed = true; } };
struct Dispatcher { bool closed = false; void close() { closed = true; } };
struct ScheduleClock {
  bool stopped = false, joined = false;
  void request_stop() { stopped = true; }
  bool joinable() const { return !joined; }
  void join() { assert(stopped); joined = true; }
};
struct BridgeContext {
  webview_t view = nullptr;
  std::shared_ptr<WindowEventLifetime> lifetime;
  std::shared_ptr<std::atomic_bool> window_alive = std::make_shared<std::atomic_bool>(true);
  std::shared_ptr<Replies> replies = std::make_shared<Replies>();
  std::shared_ptr<Dispatcher> dispatcher = std::make_shared<Dispatcher>();
  ScheduleClock schedule_clock;
  Router router;
  ${active}
};
${pendingEvent}
std::deque<std::function<void()>> callbacks;
int evaluated = 0, invoked = 0, destroyed = 0;
void webview_eval(webview_t current, const char*) {
  assert(current->owner->active()); ++evaluated;
}
void event_callback(webview_t current, void* value) { ${queuedEvent} }
void rpc_handler(const char* id, const char* request, void* context_pointer) {
  ${rpcGuard}
  ++invoked;
}
void webview_destroy(webview_t current) {
  // Destruction must never pump callbacks under the shared event mutex.
  assert(current->owner->lifetime->mutex.try_lock());
  current->owner->lifetime->mutex.unlock();
  assert(!current->owner->window_alive->load());
  assert(current->owner->replies->closed && current->owner->dispatcher->closed);
  while (!callbacks.empty()) {
    auto callback = std::move(callbacks.front()); callbacks.pop_front(); callback();
  }
  ++destroyed; delete current;
}
${destroy}
${retire}
void queue_event(BridgeContext& context) {
  auto* event = new PendingEvent{"script", context.lifetime, context.window_alive};
  auto* view = context.view;
  callbacks.push_back([view, event] { event_callback(view, event); });
}
int main() {
  BridgeContext main;
  main.lifetime = std::make_shared<WindowEventLifetime>();
  main.view = new Window{&main};
  // Each real RPC prerequisite must reject before reaching the handler body.
  auto replies = main.replies;
  main.replies.reset();
  rpc_handler("id", "request", &main);
  main.replies = std::move(replies);
  rpc_handler(nullptr, "request", &main);
  // Exercise window and application gates independently for both callback kinds.
  main.window_alive->store(false);
  rpc_handler("id", "request", &main);
  queue_event(main); callbacks.front()(); callbacks.pop_front();
  main.window_alive->store(true);
  main.lifetime->alive.store(false);
  rpc_handler("id", "request", &main);
  queue_event(main); callbacks.front()(); callbacks.pop_front();
  main.lifetime->alive.store(true);
  assert(invoked == 0 && evaluated == 0);
  // The same extracted guards must admit normal work before shutdown.
  rpc_handler("id", "request", &main);
  queue_event(main); callbacks.front()(); callbacks.pop_front();
  assert(invoked == 1 && evaluated == 1);
  std::vector<std::unique_ptr<BridgeContext>> children;
  auto child = std::make_unique<BridgeContext>();
  child->lifetime = main.lifetime; child->dispatcher = main.dispatcher;
  child->view = new Window{child.get()};
  auto* child_context = child.get();
  children.push_back(std::move(child));
  callbacks.push_back([&main] { rpc_handler("id", "request", &main); });
  callbacks.push_back([child_context] { rpc_handler("id", "request", child_context); });
  queue_event(main); queue_event(*child_context);
  auto retired = retire_auxiliary_windows(main, children);
  assert(children.empty() && retired.size() == 1 && !main.active());
  for (auto& current : retired) destroy_bridge_window(*current);
  assert(invoked == 1 && evaluated == 1 && destroyed == 1);
  // A late queued callback from an already destroyed child owns its gate.
  auto* late = new PendingEvent{"late", main.lifetime, child_context->window_alive};
  retired.clear();
  callbacks.push_back([late] { event_callback(nullptr, late); });
  destroy_bridge_window(main);
  assert(invoked == 1 && evaluated == 1 && destroyed == 2 && callbacks.empty());
}
`;
  const scratch = mkdtempSync(join(tmpdir(), "yukinal-native-lifecycle-"));
  try {
    const source = join(scratch, "lifecycle.cpp");
    const executable = join(scratch, process.platform === "win32" ? "lifecycle.exe" : "lifecycle");
    writeFileSync(source, harness);
    const built = spawnSync(compiler, ["-std=c++20", "-pthread", source, "-o", executable], { encoding: "utf8", timeout: 30_000 });
    assert.equal(built.status, 0, built.stderr || built.error?.message || "Native lifecycle harness did not compile");
    const run = spawnSync(executable, [], {
      encoding: "utf8", timeout: 5_000,
      env: { ...process.env, PATH: `${dirname(compiler)}${process.platform === "win32" ? ";" : ":"}${process.env.PATH || ""}` },
    });
    assert.equal(run.status, 0, run.stderr || run.error?.message || `native harness exited with ${run.signal}`);
  } finally {
    assert.equal(dirname(scratch), tmpdir());
    rmSync(scratch, { recursive: true, force: true });
  }
});

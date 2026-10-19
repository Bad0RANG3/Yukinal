#include "yukinal/desktop_host/ipc_dispatcher.hpp"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdlib>
#include <deque>
#include <iostream>
#include <mutex>
#include <stdexcept>
#include <thread>
#include <vector>

#ifdef _WIN32
#ifndef NOMINMAX
#define NOMINMAX
#endif
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>
#endif

using namespace yukinal::desktop_host;
using namespace std::chrono_literals;

namespace {

void check(bool condition, const char* message) {
  if (condition) return;
  std::cerr << message << '\n';
  std::exit(1);
}

struct Gate {
  std::mutex mutex;
  std::condition_variable ready;
  bool opened = false;
  void open() { { std::lock_guard lock(mutex); opened = true; } ready.notify_all(); }
  void wait() {
    std::unique_lock lock(mutex);
    check(ready.wait_for(lock, 5s, [&] { return opened; }), "local test gate timed out");
  }
};

struct UiQueue {
  std::mutex mutex;
  std::condition_variable ready;
  std::deque<IpcReplyTarget::UiTask> tasks;
  bool post(IpcReplyTarget::UiTask task) {
    { std::lock_guard lock(mutex); tasks.push_back(std::move(task)); }
    ready.notify_all();
    return true;
  }
  void await(std::size_t count) {
    std::unique_lock lock(mutex);
    check(ready.wait_for(lock, 5s, [&] { return tasks.size() >= count; }), "UI reply was not posted");
  }
  void drain() {
    std::deque<IpcReplyTarget::UiTask> pending;
    { std::lock_guard lock(mutex); pending.swap(tasks); }
    for (auto& task : pending) task();
  }
};

void routing_preserves_ui_affinity() {
  for (const auto* method : {"__shell.open_workpage", "__shell.pin", "__shell.update_surface",
                             "local_path_pick", "file_drag_out_start", "shell.open_url",
                             "core_ping", "__events.subscribe", "__events.unsubscribe"}) {
    check(IpcDispatcher::route(method) == IpcDispatchRoute::ui_thread,
          "native UI/bookkeeping methods must remain on the UI thread");
  }
  check(IpcDispatcher::route("terminal_open") == IpcDispatchRoute::background,
        "process creation must remain serialized with SSH credential setup");
  check(IpcDispatcher::route("unknown_future_command") == IpcDispatchRoute::background,
        "new commands must default to serialized background execution");
}

void slow_collection_does_not_block_terminal_or_ui_replies() {
  Gate started, release;
  UiQueue ui;
  const auto ui_thread = std::this_thread::get_id();
  std::vector<std::string> replies;
  std::atomic<int> active{0}, maximum{0};
  auto target = std::make_shared<IpcReplyTarget>(
      [&](auto task) { return ui.post(std::move(task)); },
      [&](const auto& id, bool ok, const auto& body) {
        check(std::this_thread::get_id() == ui_thread, "reply sink ran outside the UI thread");
        check(ok && nlohmann::json::parse(body).contains("method"), "reply payload was corrupted");
        replies.push_back(id);
      });
  IpcDispatcher dispatcher([&](const auto& method, auto) -> IpcReply {
    check(std::this_thread::get_id() != ui_thread, "slow invoke ran on the UI thread");
    if (method == "terminal_write") return {true, {{"method", method}}};
    const int count = ++active;
    maximum.store(std::max(maximum.load(), count));
    if (method == "server_snapshot") { started.open(); release.wait(); }
    --active;
    return {true, {{"method", method}}};
  }, {2, 2, 1024});
  const auto completion = [target](std::string id) {
    return [target, id = std::move(id)](IpcReply reply) { (void)target->complete(id, std::move(reply)); };
  };
  check(dispatcher.submit("server_snapshot", {{"serverId", "srv_local"}}, completion("snapshot"), 32)
            == IpcSubmitResult::accepted, "snapshot was not accepted");
  started.wait();
  check(dispatcher.submit("server_snapshot", {{"serverId", "srv_local"}}, completion("duplicate"), 32)
            == IpcSubmitResult::accepted, "identical collection should share the in-flight request");
  check(dispatcher.submit("server_services", {{"serverId", "srv_local"}}, completion("services"), 32)
            == IpcSubmitResult::accepted, "pending services were not accepted");
  check(dispatcher.submit("server_logs", {{"serverId", "srv_local"}}, completion("logs"), 32)
            == IpcSubmitResult::accepted, "pending logs were not accepted");
  check(dispatcher.submit("server_connect", {}, completion("overflow"), 32)
            == IpcSubmitResult::busy, "background queue limit was not enforced");
  check(dispatcher.submit("terminal_write", {{"data", "x"}}, completion("terminal"), 32)
            == IpcSubmitResult::accepted, "terminal was starved by the slow snapshot");
  ui.await(1);
  ui.drain();
  check(replies == std::vector<std::string>{"terminal"},
        "terminal reply should arrive while the snapshot is still blocked");
  check(dispatcher.submit("local_path_pick", {}, completion("picker"), 32)
            == IpcSubmitResult::requires_ui_thread, "dispatcher executed a UI-affine command");
  release.open();
  ui.await(4);
  ui.drain();
  check(replies == std::vector<std::string>({"terminal", "snapshot", "duplicate", "services", "logs"}),
        "serialized commands must retain FIFO response order");
  check(maximum == 1, "ordinary router invokes ran concurrently");
  dispatcher.close();
  dispatcher.join();
}

void collection_fanout_is_bounded_and_parameter_exact() {
  Gate started, release;
  UiQueue ui;
  std::atomic<int> valid_calls{0}, invalid_calls{0};
  std::vector<std::string> replies;
  IpcReplyTarget target([&](auto task) { return ui.post(std::move(task)); },
      [&](const auto& id, bool ok, const auto& body) {
        const auto value = nlohmann::json::parse(body);
        if (id == "invalid") {
          check(!ok && value.at("code") == "INVALID_PARAMS", "invalid params received a shared success");
        } else {
          check(ok && value.at("sample") == 42, "coalesced callers did not receive the same result");
        }
        replies.push_back(id);
      });
  IpcDispatcher dispatcher([&](const auto&, auto params) -> IpcReply {
    if (params.size() != 1) {
      ++invalid_calls;
      return {false, {{"code", "INVALID_PARAMS"}}};
    }
    ++valid_calls;
    started.open();
    release.wait();
    return {true, {{"sample", 42}}};
  }, {1, 1, 64, 3});
  const auto done = [&](std::string id) {
    return [&, id = std::move(id)](auto reply) { (void)target.complete(id, std::move(reply)); };
  };
  const auto params = nlohmann::json{{"serverId", "srv_local"}};
  check(dispatcher.submit("server_snapshot", params, done("first"), 16) == IpcSubmitResult::accepted,
        "initial coalesced request was rejected");
  started.wait();
  check(dispatcher.submit("server_snapshot", params, done("second"), 16) == IpcSubmitResult::accepted &&
        dispatcher.submit("server_snapshot", params, done("third"), 16) == IpcSubmitResult::accepted,
        "bounded coalesced waiters were rejected");
  check(dispatcher.submit("server_snapshot", params, done("excess"), 1) == IpcSubmitResult::busy,
        "collection fanout exceeded its waiter limit");
  // A separate terminal queue has space, so only aggregate waiter bytes can
  // reject this request (48 bytes are already held by the three waiters).
  check(dispatcher.submit("terminal_resize", {}, {}, 17) == IpcSubmitResult::busy,
        "coalesced waiters were not counted toward the payload budget");
  check(dispatcher.submit("server_snapshot", {{"serverId", "srv_local"}, {"unexpected", true}},
                          done("invalid"), 16) == IpcSubmitResult::accepted,
        "different params must receive independent router validation");
  release.open();
  ui.await(4);
  ui.drain();
  dispatcher.close();
  dispatcher.join();
  check(valid_calls == 1 && invalid_calls == 1,
        "fanout performed duplicate work or bypassed invalid parameter validation");
  check(replies == std::vector<std::string>({"first", "second", "third", "invalid"}),
        "bounded completion fanout lost or duplicated a caller");
}

void terminal_fifo_and_byte_limit_are_bounded() {
  Gate started, release, finished;
  std::mutex mutex;
  std::vector<std::string> order;
  IpcDispatcher dispatcher([&](const auto& method, auto) -> IpcReply {
    if (method == "terminal_write") { started.open(); release.wait(); }
    { std::lock_guard lock(mutex); order.push_back(method); }
    return {true, {}};
  }, {1, 1, 10});
  check(dispatcher.submit("terminal_write", {}, {}, 8) == IpcSubmitResult::accepted,
        "initial terminal operation should be accepted");
  started.wait();
  check(dispatcher.submit("server_logs", {}, {}, 3) == IpcSubmitResult::busy,
        "in-flight payload bytes must count toward the shared memory limit");
  check(dispatcher.submit("terminal_resize", {}, [&](auto) { finished.open(); }, 2)
            == IpcSubmitResult::accepted, "terminal queue should accept its bounded pending item");
  check(dispatcher.submit("terminal_close", {}, {}, 1) == IpcSubmitResult::busy,
        "terminal queue limit was not enforced");
  release.open();
  finished.wait();
  dispatcher.close();
  dispatcher.join();
  check(order == std::vector<std::string>({"terminal_write", "terminal_resize"}),
        "terminal input and resize must remain ordered");
}

void closing_discards_queued_and_late_callbacks_without_destroying_active_state() {
  Gate started, release;
  UiQueue ui;
  std::atomic<int> queued_calls{0}, replies{0};
  auto target = std::make_shared<IpcReplyTarget>(
      [&](auto task) { return ui.post(std::move(task)); },
      [&](const auto&, bool, const auto&) { ++replies; });
  auto owner = std::make_shared<int>(42);
  std::weak_ptr<int> weak = owner;
  {
    IpcDispatcher dispatcher([&, retained = owner](const auto& method, auto) -> IpcReply {
      check(*retained == 42, "active task lost its router/database owner");
      if (method == "slow") { started.open(); release.wait(); }
      else ++queued_calls;
      return {true, {}};
    });
    owner.reset();
    check(dispatcher.submit("slow", {}, [target](auto reply) {
      (void)target->complete("late", std::move(reply));
    }) == IpcSubmitResult::accepted, "slow task was not accepted");
    started.wait();
    check(dispatcher.submit("queued", {}, {}) == IpcSubmitResult::accepted,
          "pending task was not accepted");
    check(target->complete("already-posted", {true, {}}), "reply was not queued for UI");
    target->close();
    target.reset();
    dispatcher.close();  // Must return while `slow` is still waiting.
    check(dispatcher.submit("after-close", {}, {}) == IpcSubmitResult::closed,
          "closing dispatcher accepted more work");
    check(!weak.expired(), "running task ownership ended before join");
    release.open();
    dispatcher.join();
    ui.drain();  // Includes a callback posted before the window closed.
    check(queued_calls == 0 && replies == 0,
          "closed windows must not run pending operations or receive stale replies");
  }
  check(weak.expired(), "dispatcher leaked task ownership after join");
}

void exceptions_do_not_kill_workers_or_expose_request_values() {
  UiQueue ui;
  std::vector<std::string> codes;
  IpcReplyTarget target([&](auto task) { return ui.post(std::move(task)); },
      [&](const auto&, bool ok, const auto& body) {
        const auto value = nlohmann::json::parse(body);
        check(body.find("private-test-value") == std::string::npos, "exception leaked request contents");
        codes.push_back(ok ? "ok" : value.at("code").template get<std::string>());
      });
  IpcDispatcher dispatcher([](const auto& method, auto) -> IpcReply {
    if (method == "throws") throw std::runtime_error("private-test-value");
    return {true, {}};
  });
  for (const auto* method : {"throws", "next"}) {
    check(dispatcher.submit(method, {}, [&](auto reply) { (void)target.complete("id", std::move(reply)); })
              == IpcSubmitResult::accepted, "exception test task was not accepted");
  }
  ui.await(2);
  ui.drain();
  dispatcher.close();
  dispatcher.join();
  check(codes == std::vector<std::string>({"INTERNAL_ERROR", "ok"}),
        "worker did not recover after an exception");
}

#ifdef _WIN32
void closing_cancels_a_real_blocked_terminal_pipe_write() {
  HANDLE read = nullptr, write = nullptr;
  check(CreatePipe(&read, &write, nullptr, 4096) != FALSE, "could not create local regression pipe");
  std::atomic<bool> aborted{false};
  std::atomic<int> replies{0};
  IpcDispatcher dispatcher([&](const auto&, auto) -> IpcReply {
    // With no pipe reader this fills the 4KiB buffer and blocks in the same
    // synchronous Windows API as TerminalManager::write_frame.
    const std::string payload(8 * 1024 * 1024, 'x');
    DWORD written = 0;
    const auto ok = WriteFile(write, payload.data(), static_cast<DWORD>(payload.size()), &written, nullptr);
    aborted.store(!ok && GetLastError() == ERROR_OPERATION_ABORTED);
    return {true, {}};
  });
  check(dispatcher.submit("terminal_write", {}, [&](auto) { ++replies; }) == IpcSubmitResult::accepted,
        "blocked-pipe request was rejected");
  const auto deadline = std::chrono::steady_clock::now() + 3s;
  DWORD buffered = 0;
  while (buffered == 0 && std::chrono::steady_clock::now() < deadline) {
    check(PeekNamedPipe(read, nullptr, 0, nullptr, &buffered, nullptr) != FALSE,
          "could not observe local pipe saturation");
    if (buffered == 0) std::this_thread::sleep_for(1ms);
  }
  check(buffered > 0, "terminal test did not enter a blocked kernel pipe write");
  const auto before = std::chrono::steady_clock::now();
  dispatcher.close();
  dispatcher.join();
  check(std::chrono::steady_clock::now() - before < 2s,
        "closing waited indefinitely for the terminal pipe reader");
  check(aborted && replies == 0, "blocked pipe was not cancelled or delivered a stale reply");
  CloseHandle(write);
  CloseHandle(read);
}
#endif

}  // namespace

int main() {
  routing_preserves_ui_affinity();
  slow_collection_does_not_block_terminal_or_ui_replies();
  collection_fanout_is_bounded_and_parameter_exact();
  terminal_fifo_and_byte_limit_are_bounded();
  closing_discards_queued_and_late_callbacks_without_destroying_active_state();
  exceptions_do_not_kill_workers_or_expose_request_values();
#ifdef _WIN32
  closing_cancels_a_real_blocked_terminal_pipe_write();
#endif
  std::cout << "IPC dispatcher tests passed (slow work, FIFO, bounds, UI affinity, close, exceptions).\n";
}

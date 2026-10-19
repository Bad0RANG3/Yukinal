#pragma once

#include "yukinal/desktop_host/ipc_router.hpp"

#include <cstddef>
#include <functional>
#include <memory>
#include <string>
#include <string_view>

namespace yukinal::desktop_host {

enum class IpcDispatchRoute { ui_thread, background, terminal_control };
enum class IpcSubmitResult { accepted, busy, closed, requires_ui_thread };

struct IpcDispatchLimits {
  std::size_t background_queue = 16;
  std::size_t terminal_queue = 128;
  std::size_t pending_bytes = 32 * 1024 * 1024;
  std::size_t collection_waiters = 16;
};

// Two fixed workers, not a general-purpose concurrent router pool. All ordinary
// invokes stay serialized to protect router state and operation ordering.
// Only the audited, independently synchronized PTY control methods use the other
// FIFO. Native dialogs/window operations and short bridge bookkeeping stay on UI.
class IpcDispatcher {
 public:
  using Invoke = std::function<IpcReply(const std::string&, nlohmann::json)>;
  // Nonblocking completion; it may enqueue a reply, but must not join workers.
  using Complete = std::function<void(IpcReply)>;

  explicit IpcDispatcher(Invoke invoke, IpcDispatchLimits limits = {});
  ~IpcDispatcher();
  IpcDispatcher(const IpcDispatcher&) = delete;
  IpcDispatcher& operator=(const IpcDispatcher&) = delete;

  [[nodiscard]] static IpcDispatchRoute route(std::string_view method);
  // Identical, strictly serverId-only collection reads share one invoke, with
  // bounded completion fanout. Every waiter still consumes the byte budget.
  [[nodiscard]] IpcSubmitResult submit(std::string method, nlohmann::json params,
                                       Complete complete, std::size_t request_bytes = 0);
  // Stop admission and discard queued work/replies without waiting for I/O.
  void close();
  // Call after destroying the windows. Ordinary operations retain their bounded
  // transport/RPC deadlines; Windows terminal pipe I/O is cancelled on close.
  // The router/database must outlive this join.
  void join();

 private:
  struct Impl;
  std::unique_ptr<Impl> impl_;
};

// A window-independent lifetime gate. `post` must enqueue asynchronously onto
// the UI thread; `sink` runs there. Closing the target synchronizes with posting
// and invalidates already queued callbacks before WebView destruction.
class IpcReplyTarget {
 public:
  using UiTask = std::function<void()>;
  using Post = std::function<bool(UiTask)>;
  using Sink = std::function<void(const std::string&, bool, const std::string&)>;

  IpcReplyTarget(Post post, Sink sink);
  ~IpcReplyTarget();
  IpcReplyTarget(const IpcReplyTarget&) = delete;
  IpcReplyTarget& operator=(const IpcReplyTarget&) = delete;

  [[nodiscard]] bool complete(std::string id, IpcReply reply) noexcept;
  void close();

 private:
  struct State;
  std::shared_ptr<State> state_;
};

}  // namespace yukinal::desktop_host

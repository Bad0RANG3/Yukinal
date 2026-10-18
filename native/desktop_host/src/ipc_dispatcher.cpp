#include "yukinal/desktop_host/ipc_dispatcher.hpp"

#include <array>
#include <atomic>
#include <condition_variable>
#include <deque>
#include <mutex>
#include <map>
#include <stdexcept>
#include <thread>
#include <utility>
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

namespace yukinal::desktop_host {
namespace {

std::string collection_key(const std::string& method, const nlohmann::json& params) {
  if (method != "server_snapshot" && method != "server_logs" && method != "server_services") {
    return {};
  }
  // Only this exact read-only shape may share results. Invalid/extended params
  // must receive their own router validation, never another caller's success.
  if (!params.is_object() || params.size() != 1 || !params.contains("serverId") ||
      !params.at("serverId").is_string()) {
    return {};
  }
  const auto& id = params.at("serverId").get_ref<const std::string&>();
  if (id.empty() || id.size() > 256) return {};
  return method + ":" + id;
}

IpcReply internal_error() {
  return {false, {{"code", "INTERNAL_ERROR"}, {"message", "Desktop IPC request failed"}}};
}

}  // namespace

struct IpcDispatcher::Impl {
  struct Job {
    std::string method;
    nlohmann::json params;
    std::vector<Complete> completions;
    std::string collection;
    std::size_t bytes = 0;
  };

  Invoke invoke;
  IpcDispatchLimits limits;
  std::mutex mutex;
  std::condition_variable ready;
  // Protect completion admission separately, so submit remains reentrant from a
  // completion. close() never waits for an active network invoke.
  std::recursive_mutex completion_mutex;
  std::atomic_bool stopping{false};
  std::array<std::deque<std::shared_ptr<Job>>, 2> queues;
  std::map<std::string, std::shared_ptr<Job>> collections;
  std::size_t pending_bytes = 0;
  std::vector<std::thread> workers;
#ifdef _WIN32
  // Use real duplicated Windows handles, not std::thread::native_handle(),
  // which is a pthread identifier with the supported MinGW toolchain.
  std::array<HANDLE, 2> worker_handles{};
  std::size_t registered_workers = 0;
  bool handle_error = false;
#endif

  Impl(Invoke call, IpcDispatchLimits bounds) : invoke(std::move(call)), limits(bounds) {
    if (!invoke || limits.background_queue == 0 || limits.terminal_queue == 0 ||
        limits.pending_bytes == 0 || limits.collection_waiters == 0) {
      throw std::invalid_argument("invalid desktop IPC dispatcher configuration");
    }
  }

  void start() {
    try {
      workers.reserve(2);
      for (std::size_t lane = 0; lane < queues.size(); ++lane) {
        workers.emplace_back([this, lane] { run(lane); });
      }
#ifdef _WIN32
      std::unique_lock lock(mutex);
      ready.wait(lock, [&] { return registered_workers == queues.size(); });
      if (handle_error) throw std::runtime_error("could not initialize desktop IPC worker handles");
#endif
    } catch (...) {
      close();
      join();
      throw;
    }
  }

  void close() {
    std::lock_guard completion_lock(completion_mutex);
    {
      std::lock_guard lock(mutex);
      stopping.store(true);
      for (auto& [key, job] : collections) job->completions.clear();
      collections.clear();
      for (auto& queue : queues) queue.clear();
      pending_bytes = 0;
    }
#ifdef _WIN32
    cancel_terminal_io();
#endif
    ready.notify_all();
  }

#ifdef _WIN32
  void cancel_terminal_io() {
    HANDLE control = nullptr;
    { std::lock_guard lock(mutex); control = worker_handles[1]; }
    // Only the audited terminal-control worker may block in a synchronous pipe
    // write. Never interrupt ordinary work in the middle of a DB/file commit.
    if (control != nullptr) (void)CancelSynchronousIo(control);
  }
#endif

  void join() {
    for (std::size_t lane = 0; lane < workers.size(); ++lane) {
      auto& worker = workers[lane];
#ifdef _WIN32
      HANDLE handle = nullptr;
      { std::lock_guard lock(mutex); handle = worker_handles[lane]; }
      // Retry cancellation while joining: close() may race the instant before
      // WriteFile enters the kernel, when the first cancellation finds no I/O.
      if (worker.joinable() && handle != nullptr) {
        while (WaitForSingleObject(handle, 50) == WAIT_TIMEOUT) {
          if (stopping.load()) cancel_terminal_io();
        }
      }
#endif
      if (worker.joinable()) worker.join();
#ifdef _WIN32
      {
        std::lock_guard lock(mutex);
        if (worker_handles[lane] != nullptr) CloseHandle(worker_handles[lane]);
        worker_handles[lane] = nullptr;
      }
#endif
    }
  }

  void run(std::size_t lane) {
#ifdef _WIN32
    HANDLE handle = nullptr;
    (void)DuplicateHandle(GetCurrentProcess(), GetCurrentThread(), GetCurrentProcess(),
                          &handle, 0, FALSE, DUPLICATE_SAME_ACCESS);
    {
      std::lock_guard lock(mutex);
      worker_handles[lane] = handle;
      if (handle == nullptr) handle_error = true;
      ++registered_workers;
    }
    ready.notify_all();
    if (handle == nullptr) return;
#endif
    for (;;) {
      std::shared_ptr<Job> job;
      {
        std::unique_lock lock(mutex);
        ready.wait(lock, [&] { return stopping.load() || !queues[lane].empty(); });
        if (stopping.load()) return;
        job = std::move(queues[lane].front());
        queues[lane].pop_front();
      }
      IpcReply result;
      try {
        result = invoke(job->method, std::move(job->params));
      } catch (const nlohmann::json::exception&) {
        result = {false, {{"code", "INVALID_REQUEST"}, {"message", "Desktop IPC request is invalid"}}};
      } catch (...) {
        // Do not expose arbitrary exception text or request/credential values.
        result = internal_error();
      }
      std::vector<Complete> completions;
      {
        std::lock_guard lock(mutex);
        if (!stopping.load()) {
          pending_bytes -= job->bytes;
          if (!job->collection.empty()) collections.erase(job->collection);
          completions = std::move(job->completions);
        }
      }
      std::lock_guard completion_lock(completion_mutex);
      for (std::size_t index = 0; index < completions.size() && !stopping.load(); ++index) {
        if (!completions[index]) continue;
        try {
          completions[index](index + 1 == completions.size() ? std::move(result) : result);
        } catch (...) {}
      }
    }
  }
};

IpcDispatcher::IpcDispatcher(Invoke invoke, IpcDispatchLimits limits)
    : impl_(std::make_unique<Impl>(std::move(invoke), limits)) {
  impl_->start();
}

IpcDispatcher::~IpcDispatcher() {
  close();
  join();
}

IpcDispatchRoute IpcDispatcher::route(std::string_view method) {
  if (method.starts_with("__shell.") || method == "local_path_pick" ||
      method == "file_drag_out_start" || method == "shell.open_url" ||
      method == "core_ping" || method == "__events.subscribe" || method == "__events.unsubscribe") {
    return IpcDispatchRoute::ui_thread;
  }
  // These never create processes or change process credentials. TerminalManager
  // protects its session map and pipes; FIFO preserves write/resize/close order.
  if (method == "terminal_write" || method == "terminal_resize" || method == "terminal_close") {
    return IpcDispatchRoute::terminal_control;
  }
  return IpcDispatchRoute::background;
}

IpcSubmitResult IpcDispatcher::submit(std::string method, nlohmann::json params,
                                    Complete complete, std::size_t request_bytes) {
  const auto selected = route(method);
  if (selected == IpcDispatchRoute::ui_thread) return IpcSubmitResult::requires_ui_thread;
  const std::size_t lane = selected == IpcDispatchRoute::terminal_control ? 1 : 0;
  const auto collection = collection_key(method, params);
  if (request_bytes == 0) request_bytes = method.size() + params.dump().size();
  {
    std::lock_guard lock(impl_->mutex);
    if (impl_->stopping.load()) return IpcSubmitResult::closed;
    const auto limit = lane == 0 ? impl_->limits.background_queue : impl_->limits.terminal_queue;
    if (request_bytes > impl_->limits.pending_bytes ||
        request_bytes > impl_->limits.pending_bytes - impl_->pending_bytes) {
      return IpcSubmitResult::busy;
    }
    if (const auto found = impl_->collections.find(collection); found != impl_->collections.end()) {
      auto& job = *found->second;
      if (job.completions.size() >= impl_->limits.collection_waiters) return IpcSubmitResult::busy;
      job.completions.push_back(std::move(complete));
      job.bytes += request_bytes;
      impl_->pending_bytes += request_bytes;
      return IpcSubmitResult::accepted;
    }
    if (impl_->queues[lane].size() >= limit) return IpcSubmitResult::busy;
    auto job = std::make_shared<Impl::Job>(Impl::Job{
        std::move(method), std::move(params), {}, collection, request_bytes});
    job->completions.push_back(std::move(complete));
    if (!collection.empty()) impl_->collections.emplace(collection, job);
    try {
      impl_->queues[lane].push_back(std::move(job));
    } catch (...) {
      if (!collection.empty()) impl_->collections.erase(collection);
      throw;
    }
    impl_->pending_bytes += request_bytes;
  }
  impl_->ready.notify_all();
  return IpcSubmitResult::accepted;
}

void IpcDispatcher::close() { impl_->close(); }
void IpcDispatcher::join() { impl_->join(); }

struct IpcReplyTarget::State {
  std::mutex mutex;
  bool alive = true;
  Post post;
  Sink sink;
};

IpcReplyTarget::IpcReplyTarget(Post post, Sink sink) : state_(std::make_shared<State>()) {
  if (!post || !sink) throw std::invalid_argument("invalid desktop IPC reply target");
  state_->post = std::move(post);
  state_->sink = std::move(sink);
}

IpcReplyTarget::~IpcReplyTarget() { close(); }

bool IpcReplyTarget::complete(std::string id, IpcReply reply) noexcept {
  try {
    // Potentially large result serialization happens off the UI thread too.
    std::string serialized;
    try {
      serialized = reply.value.dump();
    } catch (...) {
      reply.ok = false;
      serialized = R"({"code":"INTERNAL_ERROR","message":"Desktop IPC response is invalid"})";
    }
    const auto state = state_;
    std::lock_guard lock(state->mutex);
    if (!state->alive) return false;
    return state->post([state, id = std::move(id), ok = reply.ok, serialized] {
      std::lock_guard delivery_lock(state->mutex);
      if (state->alive) state->sink(id, ok, serialized);
    });
  } catch (...) {
    return false;
  }
}

void IpcReplyTarget::close() {
  std::lock_guard lock(state_->mutex);
  state_->alive = false;
  state_->post = {};
  state_->sink = {};
}

}  // namespace yukinal::desktop_host

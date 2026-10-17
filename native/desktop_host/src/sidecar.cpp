#include "yukinal/desktop_host/sidecar.hpp"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <deque>
#include <iomanip>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <sstream>
#include <stdexcept>
#include <thread>
#include <vector>

#ifdef _WIN32
#include <windows.h>
#else
#include <cerrno>
#include <fcntl.h>
#include <signal.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>
extern char** environ;
#endif

namespace yukinal::desktop_host {
namespace {

constexpr std::size_t kMaxFrameBytes = 24 * 1024 * 1024;
constexpr std::size_t kLogCapacity = 200;
constexpr std::size_t kLogLineBytes = 4'000;
// Host RPCs run on a small pool rather than one thread so a blocking
// `host.tool.execute` (an MCP call) does not also block the `host.tool.cancel`
// that is meant to interrupt it.
constexpr std::size_t kHostWorkerCount = 4;

std::string iso8601_now() {
  const auto now = std::chrono::system_clock::now();
  const auto time = std::chrono::system_clock::to_time_t(now);
  std::tm utc{};
#ifdef _WIN32
  gmtime_s(&utc, &time);
#else
  gmtime_r(&time, &utc);
#endif
  std::ostringstream rendered;
  rendered << std::put_time(&utc, "%Y-%m-%dT%H:%M:%SZ");
  return rendered.str();
}

std::string truncate_log_line(const std::string& value) {
  if (value.size() <= kLogLineBytes) return value;
  return value.substr(0, kLogLineBytes) + "...[truncated]";
}

}  // namespace

struct SidecarClient::Impl {
  struct PendingRequest {
    std::mutex mutex;
    std::condition_variable cv;
    bool done = false;
    bool has_error = false;
    int error_code = 0;
    std::string error_message;
    nlohmann::json result;
  };

  struct HostRequest {
    long long id = 0;
    std::string method;
    nlohmann::json params;
  };

  mutable std::mutex mutex;
  std::condition_variable exited_cv;
  std::mutex io_mutex;
  std::mutex log_mutex;

  SidecarLaunch launch;
  HostHandler host_handler;
  NotificationSink notification_sink;

  bool running = false;
  bool exited = true;
  bool host_shutdown = false;
  bool process_started = false;
  std::uint32_t pid = 0;
  std::string entry;
  std::string protocol_version;
  std::string agent_version;
  std::size_t tool_count = 0;
  std::string started_at;
  std::optional<SidecarExitInfo> last_exit;

  long long next_request_id = 1;
  std::map<long long, std::shared_ptr<PendingRequest>> pending;

  std::mutex host_mutex;
  std::condition_variable host_cv;
  std::deque<HostRequest> host_queue;
  // Cancellation tokens for in-flight host requests, keyed by JSON-RPC id.
  std::mutex cancel_mutex;
  std::map<long long, std::shared_ptr<std::atomic<bool>>> cancellations;

  std::deque<std::string> logs;
  std::string stderr_partial;

#ifdef _WIN32
  HANDLE process = nullptr;
  HANDLE stdin_write = nullptr;
  HANDLE stdout_read = nullptr;
  HANDLE stderr_read = nullptr;
#else
  int stdin_write = -1;
  int stdout_read = -1;
  int stderr_read = -1;
  pid_t child_pid = -1;
#endif

  std::thread monitor_thread;
  std::thread stdout_thread;
  std::thread stderr_thread;
  std::vector<std::thread> host_workers;

  ~Impl() { reset(); }

  // --- framing and I/O -----------------------------------------------------

  void append_log(const std::string& line) {
    std::lock_guard lock(log_mutex);
    logs.push_back(truncate_log_line(line));
    while (logs.size() > kLogCapacity) logs.pop_front();
  }

  void append_stderr_chunk(const std::string& chunk) {
    std::lock_guard lock(log_mutex);
    stderr_partial += chunk;
    std::size_t start = 0;
    for (;;) {
      const auto newline = stderr_partial.find('\n', start);
      if (newline == std::string::npos) break;
      auto line = stderr_partial.substr(start, newline - start);
      if (!line.empty() && line.back() == '\r') line.pop_back();
      if (!line.empty()) logs.push_back(truncate_log_line(line));
      start = newline + 1;
    }
    if (start > 0) stderr_partial.erase(0, start);
    while (logs.size() > kLogCapacity) logs.pop_front();
    if (stderr_partial.size() > kMaxFrameBytes) {
      logs.push_back(truncate_log_line(stderr_partial));
      stderr_partial.clear();
    }
  }

  void process_stdout_buffer(std::string& buffer) {
    std::size_t start = 0;
    for (;;) {
      const auto newline = buffer.find('\n', start);
      if (newline == std::string::npos) break;
      auto line = buffer.substr(start, newline - start);
      if (!line.empty() && line.back() == '\r') line.pop_back();
      process_frame_line(line);
      start = newline + 1;
    }
    if (start > 0) buffer.erase(0, start);
    if (buffer.size() > kMaxFrameBytes) {
      append_log("dropped oversized sidecar frame");
      buffer.clear();
    }
  }

  void process_frame_line(const std::string& line) {
    if (line.empty()) return;
    nlohmann::json frame;
    try {
      frame = nlohmann::json::parse(line);
    } catch (const nlohmann::json::exception&) {
      append_log("dropped malformed sidecar frame");
      return;
    }
    if (!frame.is_object()) return;

    if (frame.contains("method") && frame.at("method").is_string()) {
      const auto method = frame.at("method").get<std::string>();
      const auto params = frame.contains("params") ? frame.at("params") : nlohmann::json::object();
      if (frame.contains("id") && frame.at("id").is_number_integer()) {
        enqueue_host_request(frame.at("id").get<long long>(), method, params);
      } else {
        deliver_notification(method, params);
      }
      return;
    }
    if (frame.contains("id") && frame.at("id").is_number_integer() &&
        (frame.contains("result") || frame.contains("error"))) {
      complete_request(frame.at("id").get<long long>(), frame);
    }
  }

  void complete_request(long long id, const nlohmann::json& frame) {
    std::shared_ptr<PendingRequest> pending_request;
    {
      std::lock_guard lock(mutex);
      const auto found = pending.find(id);
      if (found == pending.end()) return;
      pending_request = found->second;
      pending.erase(found);
    }
    std::lock_guard lock(pending_request->mutex);
    if (frame.contains("error")) {
      const auto& error = frame.at("error");
      pending_request->has_error = true;
      pending_request->error_code =
          error.is_object() && error.contains("code") && error.at("code").is_number_integer()
              ? error.at("code").get<int>()
              : -32603;
      pending_request->error_message =
          error.is_object() && error.contains("message") && error.at("message").is_string()
              ? error.at("message").get<std::string>()
              : "The Agent sidecar request failed";
    } else {
      pending_request->result = frame.at("result");
    }
    pending_request->done = true;
    pending_request->cv.notify_all();
  }

  void enqueue_host_request(long long id, std::string method, nlohmann::json params) {
    {
      std::lock_guard lock(host_mutex);
      if (host_shutdown) return;
      host_queue.push_back(HostRequest{id, std::move(method), std::move(params)});
    }
    host_cv.notify_one();
  }

  void deliver_notification(const std::string& method, const nlohmann::json& params) {
    NotificationSink sink;
    {
      std::lock_guard lock(mutex);
      sink = notification_sink;
    }
    if (!sink) return;
    try {
      sink(method, params);
    } catch (...) {
      // A UI notification failure must not tear down the sidecar transport.
    }
  }

  bool write_line(const nlohmann::json& frame, std::string& error) {
    const auto serialized = frame.dump();
    if (serialized.size() > kMaxFrameBytes) {
      error = "The Agent sidecar frame exceeded its size limit";
      return false;
    }
    std::string payload = serialized;
    payload.push_back('\n');
    std::lock_guard lock(io_mutex);
#ifdef _WIN32
    if (stdin_write == nullptr) {
      error = "The Agent sidecar is not running";
      return false;
    }
    DWORD written = 0;
    std::size_t offset = 0;
    while (offset < payload.size()) {
      const auto remaining =
          static_cast<DWORD>(std::min<std::size_t>(payload.size() - offset, 1 << 20));
      if (!WriteFile(stdin_write, payload.data() + offset, remaining, &written, nullptr) ||
          written == 0) {
        error = "Could not write to the Agent sidecar";
        return false;
      }
      offset += written;
    }
    return true;
#else
    if (stdin_write < 0) {
      error = "The Agent sidecar is not running";
      return false;
    }
    std::size_t offset = 0;
    while (offset < payload.size()) {
      const auto written =
          ::write(stdin_write, payload.data() + offset, payload.size() - offset);
      if (written < 0) {
        if (errno == EINTR) continue;
        error = "Could not write to the Agent sidecar";
        return false;
      }
      offset += static_cast<std::size_t>(written);
    }
    return true;
#endif
  }

  // --- process management --------------------------------------------------

  void close_stdin() {
    std::lock_guard lock(io_mutex);
#ifdef _WIN32
    if (stdin_write != nullptr) {
      CloseHandle(stdin_write);
      stdin_write = nullptr;
    }
#else
    if (stdin_write >= 0) {
      close(stdin_write);
      stdin_write = -1;
    }
#endif
  }

  void cleanup_handles() {
    std::lock_guard lock(io_mutex);
#ifdef _WIN32
    if (stdout_read != nullptr) {
      CloseHandle(stdout_read);
      stdout_read = nullptr;
    }
    if (stderr_read != nullptr) {
      CloseHandle(stderr_read);
      stderr_read = nullptr;
    }
    if (process != nullptr) {
      CloseHandle(process);
      process = nullptr;
    }
#else
    if (stdout_read >= 0) {
      close(stdout_read);
      stdout_read = -1;
    }
    if (stderr_read >= 0) {
      close(stderr_read);
      stderr_read = -1;
    }
    child_pid = -1;
#endif
    process_started = false;
  }

  void terminate_process() {
#ifdef _WIN32
    if (process != nullptr) TerminateProcess(process, 1);
#else
    if (child_pid > 0) {
      kill(child_pid, SIGTERM);
      for (int attempt = 0; attempt < 20; ++attempt) {
        int status = 0;
        if (waitpid(child_pid, &status, WNOHANG) == child_pid) return;
        std::this_thread::sleep_for(std::chrono::milliseconds(50));
      }
      kill(child_pid, SIGKILL);
    }
#endif
  }

  void notify_pending_of_exit() {
    std::map<long long, std::shared_ptr<PendingRequest>> outstanding;
    {
      std::lock_guard lock(mutex);
      outstanding.swap(pending);
    }
    for (auto& item : outstanding) {
      std::lock_guard lock(item.second->mutex);
      item.second->has_error = true;
      item.second->error_code = -32603;
      item.second->error_message = "The Agent sidecar exited before answering";
      item.second->done = true;
      item.second->cv.notify_all();
    }
  }

  void monitor_process() {
    std::optional<SidecarExitInfo> exit_info;
#ifdef _WIN32
    const auto waited =
        process == nullptr ? WAIT_OBJECT_0 : WaitForSingleObject(process, INFINITE);
    if (waited == WAIT_OBJECT_0 && process != nullptr) {
      DWORD code = 0;
      if (GetExitCodeProcess(process, &code)) {
        exit_info = SidecarExitInfo{static_cast<int>(code), std::string(), iso8601_now()};
      }
    }
#else
    if (child_pid > 0) {
      int status = 0;
      if (waitpid(child_pid, &status, 0) == child_pid) {
        if (WIFEXITED(status)) {
          exit_info = SidecarExitInfo{WEXITSTATUS(status), std::string(), iso8601_now()};
        } else if (WIFSIGNALED(status)) {
          exit_info =
              SidecarExitInfo{std::nullopt, std::to_string(WTERMSIG(status)), iso8601_now()};
        }
      }
    }
#endif
    {
      std::lock_guard lock(mutex);
      running = false;
      if (exit_info.has_value()) last_exit = exit_info;
      exited = true;
    }
    exited_cv.notify_all();
    notify_pending_of_exit();
  }

  // Handles one `host.tool.cancel`: cancel the matching in-flight request and
  // answer with whether one was found. Best-effort, exactly like the retired Rust
  // host: the request may have completed between the Agent's abort and this frame.
  nlohmann::json cancel_host_request(const nlohmann::json& params) {
    long long request_id = 0;
    if (params.is_object() && params.contains("requestId") &&
        params.at("requestId").is_number_integer()) {
      request_id = params.at("requestId").get<long long>();
    }
    if (request_id <= 0) {
      throw SidecarRpcError(-32602, "host.tool.cancel requires a positive requestId");
    }
    std::shared_ptr<std::atomic<bool>> token;
    {
      std::lock_guard lock(cancel_mutex);
      const auto found = cancellations.find(request_id);
      if (found != cancellations.end()) {
        token = found->second;
        cancellations.erase(found);
      }
    }
    if (token != nullptr) token->store(true);
    return {{"cancelled", token != nullptr}};
  }

  void handle_host_frame(const HostRequest& request) {
    nlohmann::json response = {{"jsonrpc", "2.0"}, {"id", request.id}};
    try {
      if (request.method == "host.tool.cancel") {
        response["result"] = cancel_host_request(request.params);
      } else {
        auto cancel = std::make_shared<std::atomic<bool>>(false);
        {
          std::lock_guard lock(cancel_mutex);
          cancellations[request.id] = cancel;
        }
        auto forget = [this, id = request.id]() mutable {
          std::lock_guard lock(cancel_mutex);
          cancellations.erase(id);
        };
        HostHandler handler;
        {
          std::lock_guard lock(mutex);
          handler = host_handler;
        }
        if (!handler) {
          forget();
          throw SidecarRpcError(-32601, "The desktop host does not implement " + request.method);
        }
        try {
          response["result"] = handler(request.method, request.params, cancel);
        } catch (...) {
          forget();
          throw;
        }
        forget();
      }
    } catch (const SidecarRpcError& error) {
      response["error"] = {{"code", error.code()}, {"message", error.what()}};
    } catch (const std::exception& error) {
      response["error"] = {{"code", -32603}, {"message", error.what()}};
    }
    std::string ignored;
    (void)write_line(response, ignored);
  }

  void host_worker() {
    for (;;) {
      HostRequest request;
      {
        std::unique_lock lock(host_mutex);
        host_cv.wait(lock, [this] { return host_shutdown || !host_queue.empty(); });
        if (host_shutdown && host_queue.empty()) return;
        request = std::move(host_queue.front());
        host_queue.pop_front();
      }
      handle_host_frame(request);
    }
  }

  void reset() {
    close_stdin();
    // Give the Agent a short window to exit cleanly after stdin closes before
    // terminating it; a hard kill can leave its checkpoint SQLite in a bad state.
    bool exited_now = false;
    {
      std::unique_lock lock(mutex);
      exited_now = exited_cv.wait_for(lock, std::chrono::milliseconds(500),
                                      [this] { return exited; });
    }
    if (!exited_now) terminate_process();
    {
      std::lock_guard lock(host_mutex);
      host_shutdown = true;
    }
    host_cv.notify_all();
    if (monitor_thread.joinable()) monitor_thread.join();
    for (auto& worker : host_workers) {
      if (worker.joinable()) worker.join();
    }
    host_workers.clear();
    if (stdout_thread.joinable()) stdout_thread.join();
    if (stderr_thread.joinable()) stderr_thread.join();
    cleanup_handles();
    std::lock_guard lock(mutex);
    running = false;
    pid = 0;
  }
};

SidecarClient::SidecarClient() : impl_(std::make_unique<Impl>()) {}
SidecarClient::~SidecarClient() = default;

void SidecarClient::configure(SidecarLaunch launch) { impl_->launch = std::move(launch); }

void SidecarClient::set_host_handler(HostHandler handler) {
  std::lock_guard lock(impl_->mutex);
  impl_->host_handler = std::move(handler);
}

void SidecarClient::set_notification_sink(NotificationSink sink) {
  std::lock_guard lock(impl_->mutex);
  impl_->notification_sink = std::move(sink);
}

SidecarStatus SidecarClient::status() const {
  std::lock_guard lock(impl_->mutex);
  SidecarStatus result;
  result.running = impl_->running;
  if (impl_->pid != 0) result.pid = impl_->pid;
  if (!impl_->protocol_version.empty()) result.protocol_version = impl_->protocol_version;
  if (!impl_->agent_version.empty()) result.agent_version = impl_->agent_version;
  if (!impl_->entry.empty()) result.entry = impl_->entry;
  if (!impl_->started_at.empty()) result.started_at = impl_->started_at;
  result.tool_count = impl_->tool_count;
  result.last_exit = impl_->last_exit;
  return result;
}

std::vector<std::string> SidecarClient::log_lines() const {
  std::lock_guard lock(impl_->log_mutex);
  return {impl_->logs.begin(), impl_->logs.end()};
}

std::size_t SidecarClient::log_capacity() const { return kLogCapacity; }

SidecarStartResult SidecarClient::start(std::chrono::milliseconds timeout) {
  {
    std::lock_guard lock(impl_->mutex);
    if (impl_->running) {
      return SidecarStartResult{true, impl_->pid, impl_->protocol_version, impl_->agent_version,
                                impl_->entry, impl_->tool_count};
    }
  }
  if (impl_->process_started || impl_->monitor_thread.joinable()) {
    stop();
  }

  SidecarLaunch launch;
  {
    std::lock_guard lock(impl_->mutex);
    launch = impl_->launch;
  }
  if (launch.program.empty()) {
    throw SidecarError("The Python Agent sidecar program is not configured");
  }

  {
    std::lock_guard lock(impl_->mutex);
    impl_->running = true;
    impl_->exited = false;
    impl_->last_exit.reset();
    impl_->host_shutdown = false;
    impl_->process_started = true;
  }
  {
    std::lock_guard lock(impl_->host_mutex);
    impl_->host_queue.clear();
  }
  {
    std::lock_guard lock(impl_->log_mutex);
    impl_->logs.clear();
    impl_->stderr_partial.clear();
  }

  spawn_process(launch);
  impl_->monitor_thread = std::thread([this] { impl_->monitor_process(); });
  impl_->stdout_thread = std::thread([this] {
    std::string buffer;
    std::string chunk;
    while (read_stream(true, chunk)) {
      buffer += chunk;
      chunk.clear();
      impl_->process_stdout_buffer(buffer);
    }
    impl_->process_stdout_buffer(buffer);
    if (!buffer.empty() && buffer.back() == '\r') buffer.pop_back();
    impl_->process_frame_line(buffer);
  });
  impl_->stderr_thread = std::thread([this] {
    std::string chunk;
    while (read_stream(false, chunk)) {
      impl_->append_stderr_chunk(chunk);
      chunk.clear();
    }
  });
  impl_->host_workers.reserve(kHostWorkerCount);
  for (std::size_t index = 0; index < kHostWorkerCount; ++index) {
    impl_->host_workers.emplace_back([this] { impl_->host_worker(); });
  }

  try {
    const auto initialize = request(
        "initialize",
        {{"protocolVersion", "1.0"}, {"clientVersion", launch.client_version},
         {"dataDir", launch.data_directory}},
        timeout);
    const auto tools = request("tools.list", nlohmann::json::object(), timeout);
    std::size_t tool_count = 0;
    if (tools.is_object() && tools.contains("tools") && tools.at("tools").is_array()) {
      tool_count = tools.at("tools").size();
    }
    {
      std::lock_guard lock(impl_->mutex);
      impl_->protocol_version = initialize.value("protocolVersion", std::string("1.0"));
      impl_->agent_version = initialize.value("agentVersion", std::string());
      impl_->tool_count = tool_count;
    }
  } catch (...) {
    stop();
    throw;
  }
  return status_to_start_result();
}

SidecarStartResult SidecarClient::ensure_started(std::chrono::milliseconds timeout) {
  {
    std::lock_guard lock(impl_->mutex);
    if (impl_->running) {
      return SidecarStartResult{true, impl_->pid, impl_->protocol_version, impl_->agent_version,
                                impl_->entry, impl_->tool_count};
    }
  }
  return start(timeout);
}

SidecarStartResult SidecarClient::status_to_start_result() const {
  std::lock_guard lock(impl_->mutex);
  return SidecarStartResult{false, impl_->pid, impl_->protocol_version, impl_->agent_version,
                            impl_->entry, impl_->tool_count};
}

void SidecarClient::stop(std::chrono::milliseconds timeout) {
  if (!impl_->process_started) {
    // Still join any lingering threads and clear the handles.
    impl_->reset();
    return;
  }
  impl_->close_stdin();

  bool exited = false;
  {
    std::unique_lock lock(impl_->mutex);
    exited = impl_->exited_cv.wait_for(lock, timeout, [this] { return impl_->exited; });
  }
  if (!exited) {
    impl_->terminate_process();
    std::unique_lock lock(impl_->mutex);
    impl_->exited_cv.wait(lock, [this] { return impl_->exited; });
  }

  {
    std::lock_guard lock(impl_->host_mutex);
    impl_->host_shutdown = true;
  }
  impl_->host_cv.notify_all();

  if (impl_->monitor_thread.joinable()) impl_->monitor_thread.join();
  for (auto& worker : impl_->host_workers) {
    if (worker.joinable()) worker.join();
  }
  impl_->host_workers.clear();
  if (impl_->stdout_thread.joinable()) impl_->stdout_thread.join();
  if (impl_->stderr_thread.joinable()) impl_->stderr_thread.join();
  impl_->cleanup_handles();

  {
    std::lock_guard lock(impl_->mutex);
    impl_->running = false;
    impl_->pid = 0;
  }
}

nlohmann::json SidecarClient::request(const std::string& method, nlohmann::json params,
                                      std::chrono::milliseconds timeout) {
  std::shared_ptr<Impl::PendingRequest> pending_request;
  nlohmann::json frame;
  long long request_id = 0;
  {
    std::lock_guard lock(impl_->mutex);
    if (!impl_->running) throw SidecarError("The Agent sidecar is not running");
    request_id = impl_->next_request_id++;
    pending_request = std::make_shared<Impl::PendingRequest>();
    impl_->pending[request_id] = pending_request;
    frame = {{"jsonrpc", "2.0"}, {"id", request_id}, {"method", method},
             {"params", std::move(params)}};
  }

  std::string write_error;
  if (!impl_->write_line(frame, write_error)) {
    {
      std::lock_guard lock(impl_->mutex);
      impl_->pending.erase(request_id);
    }
    throw SidecarError(write_error.empty() ? "Could not write to the Agent sidecar"
                                           : write_error);
  }

  std::unique_lock lock(pending_request->mutex);
  if (!pending_request->cv.wait_for(lock, timeout, [&] { return pending_request->done; })) {
    std::lock_guard state_lock(impl_->mutex);
    impl_->pending.erase(request_id);
    throw SidecarError("The Agent sidecar request timed out");
  }
  if (pending_request->has_error) {
    throw SidecarRpcError(pending_request->error_code, pending_request->error_message);
  }
  return pending_request->result;
}

// --- platform process spawn -------------------------------------------------

void SidecarClient::spawn_process(const SidecarLaunch& launch) {
#ifdef _WIN32
  auto wide = [](const std::string& value) {
    if (value.empty()) return std::wstring();
    const auto length = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                                            static_cast<int>(value.size()), nullptr, 0);
    if (length <= 0) throw SidecarError("The Agent sidecar launch path is not valid UTF-8");
    std::wstring result(static_cast<std::size_t>(length), L'\0');
    if (MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                            static_cast<int>(value.size()), result.data(), length) <= 0) {
      throw SidecarError("The Agent sidecar launch path is not valid UTF-8");
    }
    return result;
  };

  auto quote = [](const std::wstring& value) {
    if (!value.empty() && value.find_first_of(L" \t") == std::wstring::npos) return value;
    std::wstring quoted = L"\"";
    std::size_t backslashes = 0;
    for (const auto character : value) {
      if (character == L'\\') {
        ++backslashes;
        continue;
      }
      if (character == L'"') {
        quoted.append(backslashes * 2 + 1, L'\\');
        quoted.push_back(L'"');
        backslashes = 0;
        continue;
      }
      quoted.append(backslashes, L'\\');
      backslashes = 0;
      quoted.push_back(character);
    }
    quoted.append(backslashes * 2, L'\\');
    quoted.push_back(L'"');
    return quoted;
  };

  const auto program_wide = wide(launch.program.string());
  std::wstring command_line = quote(program_wide);
  for (const auto& argument : launch.args) {
    command_line.push_back(L' ');
    command_line += quote(wide(argument));
  }

  std::vector<std::wstring> environment;
  auto append_environment = [&environment](const std::wstring& name, const std::wstring& value) {
    environment.push_back(name + L"=" + value);
  };

  // Build a minimal, explicit environment instead of forwarding the desktop
  // host's block: the Agent only needs the loader path and Python settings, and
  // every extra value is supplied by the caller.
  static const wchar_t* const kAllowedProcessNames[] = {
      L"PATH", L"PATHEXT", L"SystemRoot", L"SYSTEMDRIVE", L"WINDIR", L"TEMP", L"TMP",
      L"COMSPEC", L"USERPROFILE", L"HOMEDRIVE", L"HOMEPATH", L"APPDATA", L"LOCALAPPDATA",
      L"PROGRAMDATA", L"PROGRAMFILES", L"PROGRAMFILES(X86)", L"NUMBER_OF_PROCESSORS",
      L"PROCESSOR_ARCHITECTURE", L"OS", L"PYTHONHOME", L"PYTHONUTF8", L"PYTHONIOENCODING",
  };
  for (const auto* name : kAllowedProcessNames) {
    const auto needed = GetEnvironmentVariableW(name, nullptr, 0);
    if (needed == 0) continue;
    std::wstring value(static_cast<std::size_t>(needed), L'\0');
    const auto written = GetEnvironmentVariableW(name, value.data(), needed);
    if (written == 0 || written >= needed) continue;
    value.resize(written);
    environment.push_back(std::wstring(name) + L"=" + value);
  }
  for (const auto& pair : launch.environment) {
    append_environment(wide(pair.first), wide(pair.second));
  }
  if (!launch.data_directory.empty()) {
    append_environment(L"YUKINAL_DATA_DIR", wide(launch.data_directory));
  }
  append_environment(L"PYTHONUNBUFFERED", L"1");

  // CreateProcess requires the environment block to be sorted by name.
  std::sort(environment.begin(), environment.end(),
            [](const std::wstring& left, const std::wstring& right) {
              const auto left_eq = left.find(L'=');
              const auto right_eq = right.find(L'=');
              const auto left_name = left.substr(0, left_eq);
              const auto right_name = right.substr(0, right_eq);
              const int comparison = _wcsicmp(left_name.c_str(), right_name.c_str());
              return comparison == 0 ? left < right : comparison < 0;
            });

  std::wstring environment_block;
  for (const auto& entry : environment) {
    environment_block += entry;
    environment_block.push_back(L'\0');
  }
  environment_block.push_back(L'\0');

  SECURITY_ATTRIBUTES attributes{sizeof(SECURITY_ATTRIBUTES), nullptr, TRUE};
  HANDLE child_stdin = nullptr;
  HANDLE child_stdout = nullptr;
  HANDLE child_stderr = nullptr;
  HANDLE parent_stdin = nullptr;
  HANDLE parent_stdout = nullptr;
  HANDLE parent_stderr = nullptr;
  if (!CreatePipe(&child_stdin, &parent_stdin, &attributes, 0) ||
      !CreatePipe(&parent_stdout, &child_stdout, &attributes, 0) ||
      !CreatePipe(&parent_stderr, &child_stderr, &attributes, 0)) {
    if (child_stdin != nullptr) CloseHandle(child_stdin);
    if (parent_stdin != nullptr) CloseHandle(parent_stdin);
    if (child_stdout != nullptr) CloseHandle(child_stdout);
    if (parent_stdout != nullptr) CloseHandle(parent_stdout);
    if (child_stderr != nullptr) CloseHandle(child_stderr);
    if (parent_stderr != nullptr) CloseHandle(parent_stderr);
    throw SidecarError("The desktop host could not create the Agent sidecar pipes");
  }
  SetHandleInformation(parent_stdin, HANDLE_FLAG_INHERIT, 0);
  SetHandleInformation(parent_stdout, HANDLE_FLAG_INHERIT, 0);
  SetHandleInformation(parent_stderr, HANDLE_FLAG_INHERIT, 0);

  STARTUPINFOW startup{};
  startup.cb = sizeof(startup);
  startup.dwFlags = STARTF_USESTDHANDLES;
  startup.hStdInput = child_stdin;
  startup.hStdOutput = child_stdout;
  startup.hStdError = child_stderr;
  PROCESS_INFORMATION process_info{};
  std::wstring mutable_command_line = command_line;
  const auto working_directory =
      launch.working_directory.empty() ? std::wstring() : launch.working_directory.wstring();
  const auto created = CreateProcessW(
      program_wide.c_str(), mutable_command_line.data(), nullptr, nullptr, TRUE,
      CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT, environment_block.data(),
      working_directory.empty() ? nullptr : working_directory.c_str(), &startup,
      &process_info);
  CloseHandle(child_stdin);
  CloseHandle(child_stdout);
  CloseHandle(child_stderr);
  if (!created) {
    const auto error = GetLastError();
    CloseHandle(parent_stdin);
    CloseHandle(parent_stdout);
    CloseHandle(parent_stderr);
    throw SidecarError("The desktop host could not start the Python Agent sidecar (error " +
                       std::to_string(error) + ")");
  }
  CloseHandle(process_info.hThread);

  impl_->stdin_write = parent_stdin;
  impl_->stdout_read = parent_stdout;
  impl_->stderr_read = parent_stderr;
  impl_->process = process_info.hProcess;
  impl_->pid = process_info.dwProcessId;
  impl_->entry = launch.program.string();
  impl_->started_at = iso8601_now();
#else
  int child_stdin[2] = {-1, -1};
  int child_stdout[2] = {-1, -1};
  int child_stderr[2] = {-1, -1};
  if (pipe(child_stdin) != 0 || pipe(child_stdout) != 0 || pipe(child_stderr) != 0) {
    throw SidecarError("The desktop host could not create the Agent sidecar pipes");
  }
  const auto child = fork();
  if (child < 0) {
    throw SidecarError("The desktop host could not fork the Python Agent sidecar");
  }
  if (child == 0) {
    dup2(child_stdin[0], STDIN_FILENO);
    dup2(child_stdout[1], STDOUT_FILENO);
    dup2(child_stderr[1], STDERR_FILENO);
    close(child_stdin[0]);
    close(child_stdin[1]);
    close(child_stdout[0]);
    close(child_stdout[1]);
    close(child_stderr[0]);
    close(child_stderr[1]);
    if (!launch.data_directory.empty()) {
      setenv("YUKINAL_DATA_DIR", launch.data_directory.c_str(), 1);
    }
    setenv("PYTHONUNBUFFERED", "1", 1);
    for (const auto& pair : launch.environment) {
      setenv(pair.first.c_str(), pair.second.c_str(), 1);
    }
    std::vector<std::string> arguments;
    arguments.push_back(launch.program.string());
    for (const auto& argument : launch.args) arguments.push_back(argument);
    std::vector<char*> argv;
    for (auto& argument : arguments) argv.push_back(argument.data());
    argv.push_back(nullptr);
    if (!launch.working_directory.empty()) {
      (void)chdir(launch.working_directory.c_str());
    }
    if (launch.program.string().find('/') != std::string::npos) {
      execve(launch.program.string().c_str(), argv.data(), environ);
    } else {
      execvp(launch.program.string().c_str(), argv.data());
    }
    _exit(127);
  }
  close(child_stdin[0]);
  close(child_stdout[1]);
  close(child_stderr[1]);
  impl_->stdin_write = child_stdin[1];
  impl_->stdout_read = child_stdout[0];
  impl_->stderr_read = child_stderr[0];
  impl_->child_pid = child;
  impl_->pid = static_cast<std::uint32_t>(child);
  impl_->entry = launch.program.string();
  impl_->started_at = iso8601_now();
#endif
}

bool SidecarClient::read_stream(bool stdout_stream, std::string& chunk) {
  chunk.clear();
#ifdef _WIN32
  HANDLE handle = nullptr;
  {
    std::lock_guard lock(impl_->io_mutex);
    handle = stdout_stream ? impl_->stdout_read : impl_->stderr_read;
  }
  if (handle == nullptr) return false;
  char buffer[16 * 1024];
  DWORD read = 0;
  if (!ReadFile(handle, buffer, sizeof(buffer), &read, nullptr) || read == 0) return false;
  chunk.assign(buffer, buffer + read);
  return true;
#else
  int descriptor = -1;
  {
    std::lock_guard lock(impl_->io_mutex);
    descriptor = stdout_stream ? impl_->stdout_read : impl_->stderr_read;
  }
  if (descriptor < 0) return false;
  char buffer[16 * 1024];
  const auto read = ::read(descriptor, buffer, sizeof(buffer));
  if (read <= 0) return false;
  chunk.assign(buffer, buffer + read);
  return true;
#endif
}

}  // namespace yukinal::desktop_host

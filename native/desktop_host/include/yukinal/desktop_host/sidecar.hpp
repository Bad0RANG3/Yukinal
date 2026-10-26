#pragma once

#include <atomic>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <filesystem>
#include <functional>
#include <memory>
#include <nlohmann/json.hpp>
#include <optional>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

namespace yukinal::desktop_host {

// Raised when the sidecar answers a request with a JSON-RPC error. `code` keeps
// the numeric JSON-RPC code so callers can map stable errors back to the UI.
class SidecarRpcError : public std::runtime_error {
 public:
  SidecarRpcError(int code, std::string message)
      : std::runtime_error(std::move(message)), code_(code) {}
  [[nodiscard]] int code() const noexcept { return code_; }

 private:
  int code_;
};

// Raised for local failures (spawn, framing, timeout, shutdown).
class SidecarError : public std::runtime_error {
 public:
  using std::runtime_error::runtime_error;
};

struct SidecarLaunch {
  std::filesystem::path program;
  std::vector<std::string> args;
  std::filesystem::path working_directory;
  // Extra environment values that are merged with the host's allow-listed
  // environment. Values are never logged.
  std::vector<std::pair<std::string, std::string>> environment;
  std::string data_directory;
  std::string client_version = "2.0.1";
};

struct SidecarExitInfo {
  std::optional<int> code;
  std::string signal;
  std::string at;
};

struct SidecarStartResult {
  bool already_running = false;
  std::uint32_t pid = 0;
  std::string protocol_version;
  std::string agent_version;
  std::string entry;
  std::size_t tool_count = 0;
};

struct SidecarStatus {
  bool running = false;
  std::optional<std::uint32_t> pid;
  std::optional<std::string> protocol_version;
  std::optional<std::string> agent_version;
  std::optional<std::size_t> tool_count;
  std::string entry;
  std::optional<std::string> started_at;
  std::optional<SidecarExitInfo> last_exit;
};

// Owns the lifecycle of the Python Agent sidecar: process spawn, bounded NDJSON
// framing, JSON-RPC request/response correlation, notification delivery and the
// host RPC callbacks the Agent issues back to the desktop host.
class SidecarClient {
 public:
  // Called for an incoming `method` request from the Agent. `cancel` is set when
  // the Agent sends `host.tool.cancel` for this request's JSON-RPC id; a
  // long-running handler may poll it and return early. Throwing `SidecarRpcError`
  // answers with that error; any other exception becomes an internal error. The
  // returned value is the JSON-RPC `result`. `host.tool.cancel` itself is handled
  // by the sidecar and never reaches this handler.
  using HostHandler = std::function<nlohmann::json(
      const std::string& method, const nlohmann::json& params,
      const std::shared_ptr<std::atomic<bool>>& cancel)>;
  // Called for an incoming notification (a `method` frame without an id).
  using NotificationSink = std::function<void(const std::string& method,
                                              const nlohmann::json& params)>;

  SidecarClient();
  ~SidecarClient();
  SidecarClient(const SidecarClient&) = delete;
  SidecarClient& operator=(const SidecarClient&) = delete;

  void configure(SidecarLaunch launch);
  void set_host_handler(HostHandler handler);
  void set_notification_sink(NotificationSink sink);

  SidecarStartResult start(
      std::chrono::milliseconds timeout = std::chrono::milliseconds(20000));
  SidecarStartResult ensure_started(
      std::chrono::milliseconds timeout = std::chrono::milliseconds(20000));
  void stop(std::chrono::milliseconds timeout = std::chrono::milliseconds(3000));

  [[nodiscard]] SidecarStatus status() const;
  [[nodiscard]] std::vector<std::string> log_lines() const;
  [[nodiscard]] std::size_t log_capacity() const;

  nlohmann::json request(
      const std::string& method, nlohmann::json params = nlohmann::json::object(),
      std::chrono::milliseconds timeout = std::chrono::milliseconds(30000));

 private:
  void spawn_process(const SidecarLaunch& launch);
  bool read_stream(bool stdout_stream, std::string& chunk);
  [[nodiscard]] SidecarStartResult status_to_start_result() const;
  // Best-effort, non-throwing cleanup for a timed-out `agent.run.*` request:
  // sends `agent.run.stop` for the same run so the Python Agent stops burning
  // its budget, and never waits more than a short bounded interval.
  void cancel_timed_out_run(const std::string& method,
                            const std::optional<nlohmann::json>& run_id) noexcept;

  struct Impl;
  std::unique_ptr<Impl> impl_;
};

}  // namespace yukinal::desktop_host

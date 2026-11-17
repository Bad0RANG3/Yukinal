#include "yukinal/http_service.hpp"
#include "yukinal/ssh_security.hpp"

#include <algorithm>
#include <atomic>
#include <cerrno>
#include <chrono>
#include <cstdint>
#include <exception>
#include <limits>
#include <mutex>
#include <thread>
#include <unordered_set>
#include <unordered_map>
#include <utility>

#ifdef _WIN32
#include <windows.h>
#else
#include <csignal>
#include <unistd.h>
#endif

#include <httplib.h>
#include <nlohmann/json.hpp>

namespace yukinal::ssh_service {
namespace {

using Json = nlohmann::json;

bool parent_is_alive(std::uint64_t pid) {
#ifdef _WIN32
  if (pid > static_cast<std::uint64_t>(std::numeric_limits<DWORD>::max())) return false;
  HANDLE process = OpenProcess(SYNCHRONIZE, FALSE, static_cast<DWORD>(pid));
  if (!process) return false;
  const DWORD state = WaitForSingleObject(process, 0);
  CloseHandle(process);
  return state == WAIT_TIMEOUT;
#else
  if (pid > static_cast<std::uint64_t>(std::numeric_limits<pid_t>::max())) return false;
  if (kill(static_cast<pid_t>(pid), 0) == 0) return true;
  return errno == EPERM;
#endif
}

bool constant_time_equal(const std::string& left, const std::string& right) {
  const std::size_t count = std::max(left.size(), right.size());
  std::uint64_t difference = static_cast<std::uint64_t>(left.size() ^ right.size());
  for (std::size_t i = 0; i < count; ++i) {
    const unsigned char a = i < left.size() ? static_cast<unsigned char>(left[i]) : 0;
    const unsigned char b = i < right.size() ? static_cast<unsigned char>(right[i]) : 0;
    difference |= static_cast<std::uint64_t>(a ^ b);
  }
  return difference == 0;
}

bool valid_token(const std::string& token) {
  if (token.size() < 32 || token.size() > 256) return false;
  for (const unsigned char ch : token) {
    if (!((ch >= 'A' && ch <= 'Z') || (ch >= 'a' && ch <= 'z') ||
          (ch >= '0' && ch <= '9') || ch == '.' || ch == '_' || ch == '~' || ch == '-')) {
      return false;
    }
  }
  return true;
}

void set_json(httplib::Response& response, int status, const Json& body) {
  response.status = status;
  response.set_content(body.dump(-1, ' ', false, Json::error_handler_t::replace),
                       "application/json; charset=utf-8");
  response.set_header("Cache-Control", "no-store");
  response.set_header("X-Content-Type-Options", "nosniff");
}

void set_api_error(httplib::Response& response, int status, const std::string& code,
                   const std::string& message) {
  set_json(response, status,
           Json{{"status", "failed"}, {"error", {{"code", code}, {"message", message}}}});
}

bool authorized(const httplib::Request& request, const std::string& token) {
  if (request.has_header("Origin")) return false;
  const std::string authorization = request.get_header_value("Authorization");
  constexpr char prefix[] = "Bearer ";
  if (authorization.size() <= sizeof(prefix) - 1 ||
      authorization.compare(0, sizeof(prefix) - 1, prefix) != 0) {
    return false;
  }
  return constant_time_equal(authorization.substr(sizeof(prefix) - 1), token);
}

int error_status(const ExecResult& result) { return error_status_for(result); }

}  // namespace

int error_status_for(const ExecResult& result) {
  if (result.error_code == "execution_cancelled" || result.error_code == "execution_expired") return 403;
  if (result.error_code == "audit_unavailable") return 503;
  if (result.error_code == "denied_by_policy") return 403;
  if (result.error_code == "already_exists") return 409;
  if (result.error_code == "timeout") return 504;
  if (result.error_code == "invalid_parameters") return 422;
  if (result.error_code == "concurrent_change") return 409;
  if (result.error_code == "replace_unsupported") return 501;
  if (result.error_code == "resolver_busy" || result.error_code == "resolver_unavailable") return 503;
  return 502;
}

bool serve(const ServiceConfig& config, const std::string& api_token, OperationRunner runner,
           std::optional<std::uint64_t> parent_pid) {
  if (!valid_token(api_token) || !runner) return false;

  httplib::Server server;
  // Recovery copies arrive as base64 within one JSON request: 1 MiB expands to about 1.34 MiB.
  server.set_payload_max_length(2 * 1024 * 1024);
  server.set_read_timeout(5, 0);
  server.set_write_timeout(5, 0);
  server.new_task_queue = [] { return new httplib::ThreadPool(4); };

  server.Get("/health", [](const httplib::Request&, httplib::Response& response) {
    set_json(response, 200, Json{{"status", "healthy"}});
  });

  std::mutex consumed_server_exec_mutex;
  std::unordered_set<std::string> consumed_server_exec_targets;
  std::mutex leases_mutex;
  // Retain tombstones until process exit. A cancelled/expired id can never be
  // replayed, including cancel arriving before exec. Bound memory and fail closed.
  std::unordered_map<std::string, std::shared_ptr<ExecutionLease>> leases;
  std::atomic_int active_executions{0};
  const auto valid_execution_id = [](const std::string& id) {
    return id.size() >= 16 && id.size() <= 128 && std::all_of(id.begin(), id.end(), [](unsigned char ch) {
      return (ch >= 'a' && ch <= 'z') || (ch >= 'A' && ch <= 'Z') ||
             (ch >= '0' && ch <= '9') || ch == '-' || ch == '_';
    });
  };
  server.Post("/v1/ssh/cancel", [&](const httplib::Request& request, httplib::Response& response) {
    if (!authorized(request, api_token)) { set_api_error(response, 401, "unauthorized", "A valid local service token is required"); return; }
    const auto body = Json::parse(request.body, nullptr, false);
    if (!body.is_object() || body.size() != 1 || !body.contains("execution_id") ||
        !body["execution_id"].is_string() || !valid_execution_id(body["execution_id"].get<std::string>())) {
      set_api_error(response, 422, "invalid_parameters", "A valid execution_id is required"); return;
    }
    std::lock_guard lock(leases_mutex);
    const std::string id = body["execution_id"].get<std::string>();
    if (!leases.contains(id) && leases.size() >= 65536) {
      set_api_error(response, 503, "execution_capacity", "Execution authorization capacity exhausted"); return;
    }
    auto& lease = leases[id];
    if (!lease) lease = std::make_shared<ExecutionLease>();
    lease->cancelled.store(true);
    set_json(response, 200, {{"status", "cancelled"}});
  });
  server.Post("/v1/ssh/exec", [&config, &api_token, &consumed_server_exec_mutex,
                               &consumed_server_exec_targets, &leases_mutex, &leases,
                               &valid_execution_id, &active_executions, runner = std::move(runner)](
                                  const httplib::Request& request, httplib::Response& response) {
    if (!authorized(request, api_token)) {
      set_api_error(response, 401, "unauthorized", "A valid local service token is required");
      return;
    }
    const std::string content_type = request.get_header_value("Content-Type");
    if (content_type != "application/json" && content_type.rfind("application/json;", 0) != 0) {
      set_api_error(response, 415, "unsupported_media_type", "Content-Type must be application/json");
      return;
    }

    Json body;
    try {
      body = Json::parse(request.body);
    } catch (const Json::exception&) {
      set_api_error(response, 400, "invalid_json", "Request body is not valid JSON");
      return;
    }

    ExecRequest parsed;
    ApiError parse_error;
    if (!parse_exec_request(body, &parsed, &parse_error)) {
      set_api_error(response, parse_error.http_status, parse_error.code, parse_error.message);
      return;
    }
    const auto target = config.targets.find(parsed.target_id);
    if (target == config.targets.end()) {
      set_api_error(response, 404, "unknown_target", "target_id is not present in the configured allowlist");
      return;
    }
    std::shared_ptr<ExecutionLease> lease;
    if (!body.contains("execution")) {
      set_api_error(response, 403, "execution_required", "Every SSH operation requires a single-use execution lease"); return;
    }
    if (body.contains("execution")) {
      const auto& execution = body["execution"];
      if (!execution.is_object() || execution.size() != 2 || !execution.contains("id") ||
          !execution["id"].is_string() || !valid_execution_id(execution["id"].get<std::string>()) ||
          !execution.contains("expires_at_ms") || !execution["expires_at_ms"].is_number_integer()) {
        set_api_error(response, 422, "invalid_parameters", "Execution requires a unique id and expiry"); return;
      }
      const auto milliseconds = execution["expires_at_ms"].get<std::int64_t>();
      lease = std::make_shared<ExecutionLease>();
      lease->id = execution["id"].get<std::string>();
      {
        std::lock_guard lock(leases_mutex);
        if (leases.size() >= 65536 || !leases.emplace(lease->id, lease).second) {
          set_api_error(response, 403, "execution_cancelled", "Execution authorization is cancelled or already consumed"); return;
        }
      }
      const auto now = std::chrono::system_clock::now();
      const auto now_ms = std::chrono::duration_cast<std::chrono::milliseconds>(now.time_since_epoch()).count();
      if (milliseconds <= now_ms || milliseconds > now_ms + 660000) {
        lease->cancelled.store(true);
        set_api_error(response, 403, "execution_expired", "Execution authorization is expired or exceeds its maximum lifetime"); return;
      }
      const auto expiry = std::chrono::system_clock::time_point(std::chrono::milliseconds(milliseconds));
      lease->expires = expiry;
    }
    ScopedExecutionLease execution_scope(lease);
    if (parsed.operation == "server_exec" && target->second.allow_server_exec) {
      bool first_use = false;
      {
        std::lock_guard<std::mutex> lock(consumed_server_exec_mutex);
        first_use = consumed_server_exec_targets.insert(parsed.target_id).second;
      }
      if (!first_use) {
        set_api_error(response, 409, "operation_not_allowed",
                      "the one-shot server.exec authorization has already been used");
        return;
      }
    }

    ExecResult result;
    // Reserve one HTTP worker for revocation even when remote servers hang.
    const int concurrent = active_executions.fetch_add(1);
    struct ActiveGuard { std::atomic_int& count; ~ActiveGuard() { count.fetch_sub(1); } } active_guard{active_executions};
    if (concurrent >= 3) {
      if (lease) lease->cancelled.store(true);
      set_api_error(response, 503, "execution_capacity", "SSH execution capacity exhausted"); return;
    }
    try {
      result = runner(target->second, parsed);
    } catch (const std::exception&) {
      result.error_code = "internal_error";
      result.error_message = "The SSH operation failed unexpectedly";
    } catch (...) {
      result.error_code = "internal_error";
      result.error_message = "The SSH operation failed unexpectedly";
    }
    if (lease) {
      if (!lease->valid()) {
        result.completed = false;
        result.error_code = "execution_cancelled";
        result.error_message = "Execution authorization expired or was cancelled; the channel has been closed";
      }
      lease->cancelled.store(true);
    }
    Json output{{"status", result.completed ? "succeeded" : "failed"},
                {"target_id", parsed.target_id},
                {"operation", parsed.operation},
                {"exit_code", result.completed ? Json(result.exit_code) : Json(nullptr)},
                {"stdout", result.stdout_text},
                {"stderr", result.stderr_text},
                {"duration_ms", result.duration_ms},
                {"output_truncated", result.output_truncated}};
    if (parsed.operation == "server_exec") {
      output["stdout_truncated"] = result.stdout_truncated;
      output["stderr_truncated"] = result.stderr_truncated;
    }
    if (!result.resolved_path.empty()) output["resolved_path"] = result.resolved_path;
    if (result.completed) {
      output["error"] = nullptr;
      set_json(response, 200, output);
      return;
    }
    output["error"] = {{"code", result.error_code.empty() ? "ssh_error" : result.error_code},
                       {"message", result.error_message.empty() ? "SSH operation failed" : result.error_message}};
    if (!result.missing_metadata.empty()) output["error"]["missing"] = result.missing_metadata;
    set_json(response, error_status(result), output);
  });

  if (!server.bind_to_port("127.0.0.1", config.listen_port)) return false;
  if (!parent_pid) return server.listen_after_bind();
  if (!parent_is_alive(*parent_pid)) return true;

  std::atomic_bool stop_parent_watch = false;
  std::atomic_bool parent_lost = false;
  std::thread parent_watch([&server, parent_pid, &stop_parent_watch, &parent_lost] {
    while (!stop_parent_watch.load(std::memory_order_relaxed)) {
      if (!parent_is_alive(*parent_pid)) {
        parent_lost.store(true, std::memory_order_relaxed);
        server.stop();
        return;
      }
      std::this_thread::sleep_for(std::chrono::milliseconds(250));
    }
  });
  const bool listened = server.listen_after_bind();
  stop_parent_watch.store(true, std::memory_order_relaxed);
  parent_watch.join();
  return listened || parent_lost.load(std::memory_order_relaxed);
}

}  // namespace yukinal::ssh_service

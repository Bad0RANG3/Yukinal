#include "yukinal/http_service.hpp"

#include <algorithm>
#include <cstdint>
#include <exception>
#include <utility>

#include <httplib.h>
#include <nlohmann/json.hpp>

namespace yukinal::ssh_service {
namespace {

using Json = nlohmann::json;

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

int error_status(const ExecResult& result) {
  if (result.error_code == "timeout") return 504;
  if (result.error_code == "invalid_parameters") return 422;
  if (result.error_code == "resolver_busy" || result.error_code == "resolver_unavailable") return 503;
  return 502;
}

}  // namespace

bool serve(const ServiceConfig& config, const std::string& api_token, OperationRunner runner) {
  if (!valid_token(api_token) || !runner) return false;

  httplib::Server server;
  server.set_payload_max_length(64 * 1024);
  server.set_read_timeout(5, 0);
  server.set_write_timeout(5, 0);
  server.new_task_queue = [] { return new httplib::ThreadPool(4); };

  server.Get("/health", [](const httplib::Request&, httplib::Response& response) {
    set_json(response, 200, Json{{"status", "healthy"}});
  });

  server.Post("/v1/ssh/exec", [&config, &api_token, runner = std::move(runner)](
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

    ExecResult result;
    try {
      result = runner(target->second, parsed.service, parsed.timeout_seconds);
    } catch (const std::exception&) {
      result.error_code = "internal_error";
      result.error_message = "The SSH operation failed unexpectedly";
    } catch (...) {
      result.error_code = "internal_error";
      result.error_message = "The SSH operation failed unexpectedly";
    }
    Json output{{"status", result.completed ? "succeeded" : "failed"},
                {"target_id", parsed.target_id},
                {"operation", "service_status"},
                {"exit_code", result.completed ? Json(result.exit_code) : Json(nullptr)},
                {"stdout", result.stdout_text},
                {"stderr", result.stderr_text},
                {"duration_ms", result.duration_ms},
                {"output_truncated", result.output_truncated}};
    if (result.completed) {
      output["error"] = nullptr;
      set_json(response, 200, output);
      return;
    }
    output["error"] = {{"code", result.error_code.empty() ? "ssh_error" : result.error_code},
                       {"message", result.error_message.empty() ? "SSH operation failed" : result.error_message}};
    set_json(response, error_status(result), output);
  });

  return server.listen("127.0.0.1", config.listen_port);
}

}  // namespace yukinal::ssh_service

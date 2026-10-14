#include "yukinal/policy.hpp"

#include <cstdint>
#include <regex>
#include <set>

namespace yukinal::ssh_service {
namespace {

void set_error(ApiError* error, int status, std::string code, std::string message) {
  if (!error) return;
  error->http_status = status;
  error->code = std::move(code);
  error->message = std::move(message);
}

bool has_only_keys(const nlohmann::json& value, const std::set<std::string>& allowed) {
  for (auto it = value.begin(); it != value.end(); ++it) {
    if (!allowed.contains(it.key())) return false;
  }
  return true;
}

}  // namespace

bool valid_target_id(const std::string& value) {
  static const std::regex pattern("^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$");
  return std::regex_match(value, pattern);
}

bool valid_service_name(const std::string& value) {
  static const std::regex pattern("^[A-Za-z0-9][A-Za-z0-9_.@:-]{0,127}$");
  return std::regex_match(value, pattern);
}

bool valid_sftp_path(const std::string& value) {
  return !value.empty() && value.size() <= 4096 && value.find('\0') == std::string::npos;
}

bool parse_exec_request(const nlohmann::json& value, ExecRequest* request, ApiError* error) {
  if (!value.is_object() || !has_only_keys(value, {"target_id", "operation", "parameters", "timeout_seconds"})) {
    set_error(error, 400, "invalid_request", "Request must contain only the documented fields");
    return false;
  }
  const auto target_it = value.find("target_id");
  const auto operation_it = value.find("operation");
  const auto parameters_it = value.find("parameters");
  const auto timeout_it = value.find("timeout_seconds");
  if (target_it == value.end() || !target_it->is_string() ||
      operation_it == value.end() || !operation_it->is_string() ||
      parameters_it == value.end() || !parameters_it->is_object() ||
      timeout_it == value.end() ||
      !(timeout_it->is_number_integer() || timeout_it->is_number_unsigned())) {
    set_error(error, 400, "invalid_request", "Request fields have invalid types or are missing");
    return false;
  }

  const std::string target_id = target_it->get<std::string>();
  if (!valid_target_id(target_id)) {
    set_error(error, 422, "invalid_target_id", "target_id has an invalid format");
    return false;
  }
  const std::string operation = operation_it->get<std::string>();
  std::string service;
  std::string path;
  if (operation == "service_status") {
    if (!has_only_keys(*parameters_it, {"service"})) {
      set_error(error, 422, "invalid_parameters", "service_status accepts only a service parameter");
      return false;
    }
    const auto service_it = parameters_it->find("service");
    if (service_it == parameters_it->end() || !service_it->is_string() ||
        !valid_service_name(service_it->get<std::string>())) {
      set_error(error, 422, "invalid_parameters", "service must be a valid systemd unit name");
      return false;
    }
    service = service_it->get<std::string>();
  } else if (operation == "service_inventory") {
    if (!parameters_it->empty()) {
      set_error(error, 422, "invalid_parameters", "service_inventory accepts no parameters");
      return false;
    }
  } else if (operation == "sftp_list") {
    if (!has_only_keys(*parameters_it, {"path"})) {
      set_error(error, 422, "invalid_parameters", "sftp_list accepts only a path parameter");
      return false;
    }
    const auto path_it = parameters_it->find("path");
    if (path_it == parameters_it->end() || !path_it->is_string() ||
        !valid_sftp_path(path_it->get<std::string>())) {
      set_error(error, 422, "invalid_parameters", "path must be a non-empty SFTP path of at most 4096 bytes");
      return false;
    }
    path = path_it->get<std::string>();
  } else {
    set_error(error, 422, "unsupported_operation", "The requested structured SSH operation is not enabled");
    return false;
  }
  std::uint64_t timeout = 0;
  if (timeout_it->is_number_unsigned()) {
    timeout = timeout_it->get<std::uint64_t>();
  } else {
    const auto signed_timeout = timeout_it->get<std::int64_t>();
    if (signed_timeout < 1 || signed_timeout > 30) {
      set_error(error, 422, "invalid_timeout", "timeout_seconds must be between 1 and 30");
      return false;
    }
    timeout = static_cast<std::uint64_t>(signed_timeout);
  }
  if (timeout < 1 || timeout > 30) {
    set_error(error, 422, "invalid_timeout", "timeout_seconds must be between 1 and 30");
    return false;
  }
  if (!request) {
    set_error(error, 500, "internal_error", "Request parser was not given an output value");
    return false;
  }
  request->target_id = target_id;
  request->operation = operation;
  request->service = std::move(service);
  request->path = std::move(path);
  request->timeout_seconds = static_cast<int>(timeout);
  return true;
}

}  // namespace yukinal::ssh_service

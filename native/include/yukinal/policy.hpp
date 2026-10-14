#pragma once

#include <string>

#include <nlohmann/json.hpp>

#include "yukinal/config.hpp"

namespace yukinal::ssh_service {

struct ExecRequest {
  std::string target_id;
  std::string operation;
  std::string service;
  int timeout_seconds = 10;
};

struct ApiError {
  int http_status = 400;
  std::string code;
  std::string message;
};

bool parse_exec_request(const nlohmann::json& value, ExecRequest* request, ApiError* error);
bool valid_target_id(const std::string& value);
bool valid_service_name(const std::string& value);

}  // namespace yukinal::ssh_service

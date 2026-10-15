#pragma once

#include <cstdint>
#include <array>
#include <map>
#include <optional>
#include <string>
#include <vector>

#include <nlohmann/json.hpp>

#include "yukinal/config.hpp"

namespace yukinal::ssh_service {

struct ExecRequest {
  std::string target_id;
  std::string operation;
  std::string service;
  std::string unit;
  std::string package_manager;
  std::string package;
  std::string package_version;
  std::string path;
  std::string source_path;
  std::string destination_path;
  std::string command;
  std::string workdir;
  std::map<std::string, std::string> environment;
  std::string nonce;
  std::string container;
  bool include_all_containers = false;
  std::uint32_t tail = 120;
  std::uint32_t grace_period_seconds = 10;
  std::vector<unsigned char> content;
  std::uint64_t expected_size = 0;
  std::optional<std::uint32_t> expected_modified;
  std::array<unsigned char, 32> expected_digest{};
  std::uint32_t max_bytes = 0;
  std::uint32_t max_output_bytes = 0;
  std::uint32_t timeout_ms = 0;
  std::optional<std::uint32_t> since_seconds;
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
bool valid_container_ref(const std::string& value);
bool valid_package_ref(const std::string& value);
bool valid_package_version(const std::string& value);
bool valid_package_manager(const std::string& value);

}  // namespace yukinal::ssh_service

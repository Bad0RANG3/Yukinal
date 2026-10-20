#pragma once
#include <algorithm>
#include <stdexcept>
#include <string>
#include <nlohmann/json.hpp>
#include "yukinal/snapshot_toolkit_data.hpp"

namespace yukinal::ssh_service {
inline const nlohmann::json& snapshot_toolkit_catalog() {
  static const auto catalog = nlohmann::json::parse(kSnapshotToolkitCatalog);
  return catalog;
}

inline std::string snapshot_shell_quote(const std::string& text) {
  std::string quoted = "'";
  for (char c : text) quoted += c == '\'' ? "'\"'\"'" : std::string(1, c);
  return quoted + "'";
}

inline std::string build_system_snapshot_command(const std::string& nonce) {
  if (nonce.size() != 32 || !std::all_of(nonce.begin(), nonce.end(), [](char c) {
        return (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f');
      })) throw std::invalid_argument("invalid snapshot nonce");
  // Leave room under the SSH transport's 64 KiB cap for framing.
  std::string command = "export LC_ALL=C; yk_snapshot_remaining=49152; ";
  for (const auto& probe : snapshot_toolkit_catalog().at("probes")) {
    const auto id = probe.at("id").get<std::string>();
    auto shell = probe.at("command").get<std::string>();
    const auto timeout = probe.at("timeoutSeconds").get<int>();
    // Skip potentially blocking probes on hosts without a deadline utility.
    if (timeout > 0) {
      shell = "if command -v timeout >/dev/null 2>&1; then timeout -k 1 " +
              std::to_string(timeout) + " sh -c " + snapshot_shell_quote(shell) +
              "; else (exit 127); fi";
    } else {
      shell = "( " + shell + " )";
    }
    const auto prefix = "__YK_SNAPSHOT_" + nonce + "_" + id;
    // Preserve command status and bound each section without silently parsing a
    // truncated JSON/CSV prefix as a complete result. No remote files are written.
    command += "yk_snapshot_output=$( { " + shell + "; } 2>/dev/null ); yk_snapshot_status=$?; "
               "yk_snapshot_length=${#yk_snapshot_output}; "
               "if test \"$yk_snapshot_length\" -gt 16384 || "
               "test \"$yk_snapshot_length\" -gt \"$yk_snapshot_remaining\"; then "
               "yk_snapshot_output=''; yk_snapshot_status=125; else "
               "yk_snapshot_remaining=$((yk_snapshot_remaining - yk_snapshot_length)); fi; ";
    if (id == "docker_info") command += "yk_docker_status=$yk_snapshot_status; export yk_docker_status; ";
    command += "printf '" + prefix + "_BEGIN__\\n%s\\n" + prefix +
               "_END_%s__\\n' \"$yk_snapshot_output\" \"$yk_snapshot_status\"; ";
  }
  return command;
}

inline nlohmann::json parse_system_snapshot_outputs(const std::string& output,
                                                    const std::string& nonce) {
  // Validate nonce before constructing delimiters, including for test callers.
  (void)build_system_snapshot_command(nonce);
  auto results = nlohmann::json::object();
  for (const auto& probe : snapshot_toolkit_catalog().at("probes")) {
    const auto id = probe.at("id").get<std::string>();
    const auto prefix = "__YK_SNAPSHOT_" + nonce + "_" + id;
    const auto begin = prefix + "_BEGIN__\n";
    const auto end_marker = "\n" + prefix + "_END_";
    const auto start = output.find(begin);
    if (!probe.at("required").get<bool>() && start == std::string::npos &&
        output.find(prefix + "_END_") == std::string::npos) continue;
    if (start == std::string::npos || output.find(begin, start + begin.size()) != std::string::npos)
      throw std::runtime_error("incomplete or ambiguous system snapshot");
    const auto data_start = start + begin.size();
    const auto end = output.find(end_marker, data_start);
    if (end == std::string::npos || output.find(end_marker, end + end_marker.size()) != std::string::npos)
      throw std::runtime_error("incomplete or ambiguous system snapshot");
    const auto status_start = end + end_marker.size();
    const auto status_end = output.find("__\n", status_start);
    const auto status = output.substr(status_start, status_end - status_start);
    if (status_end == std::string::npos || status.empty() || status.size() > 3 ||
        !std::all_of(status.begin(), status.end(), [](char c) { return c >= '0' && c <= '9'; }) ||
        std::stoi(status) > 255) throw std::runtime_error("invalid system snapshot status");
    results[id] = {{"exitCode", std::stoi(status)}, {"stdout", output.substr(data_start, end - data_start)}};
  }
  return results;
}
}

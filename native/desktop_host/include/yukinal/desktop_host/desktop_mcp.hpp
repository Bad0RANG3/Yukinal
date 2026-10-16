#pragma once

#include <memory>
#include <nlohmann/json.hpp>
#include <string>

namespace yukinal::desktop_host {

class DesktopDatabase;
class CredentialStore;

// Owns the stdio MCP server children. A server is spawned on demand, handshaken
// over newline-delimited JSON-RPC, asked for its tool list, and kept alive for
// tool calls. The host owns the process, the catalog and the risk classification;
// the Agent only sees the names, descriptions and schemas.
class McpSupervisor {
 public:
  McpSupervisor(DesktopDatabase* database, const CredentialStore* credentials);
  ~McpSupervisor();
  McpSupervisor(const McpSupervisor&) = delete;
  McpSupervisor& operator=(const McpSupervisor&) = delete;

  // Builds the catalog for the Agent: one entry per enabled server that has at
  // least one reviewed tool, with the effective risk the host computed.
  [[nodiscard]] nlohmann::json catalog();
  // Explicit lifecycle for the settings UI.
  [[nodiscard]] nlohmann::json start(const std::string& server_id);
  [[nodiscard]] nlohmann::json stop(const std::string& server_id);
  // Executes one internal `mcp.<segment>.<tool>` call.
  [[nodiscard]] nlohmann::json call(const std::string& internal_name,
                                    const nlohmann::json& arguments);

 private:
  struct Impl;
  std::unique_ptr<Impl> impl_;
};

}  // namespace yukinal::desktop_host

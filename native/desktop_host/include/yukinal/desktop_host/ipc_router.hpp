#pragma once

#include "yukinal/desktop_host/credential_store.hpp"
#include "yukinal/desktop_host/database.hpp"
#include "yukinal/desktop_host/desktop_mcp.hpp"
#include "yukinal/desktop_host/desktop_terminal.hpp"
#include "yukinal/desktop_host/local_paths.hpp"
#include "yukinal/desktop_host/sidecar.hpp"

#include <atomic>
#include <chrono>
#include <functional>
#include <map>
#include <memory>
#include <mutex>
#include <nlohmann/json.hpp>
#include <optional>
#include <set>
#include <string>
#include <unordered_map>
#include <utility>
#include <vector>

namespace yukinal::desktop_host {

struct IpcReply {
  bool ok;
  nlohmann::json value;
};

class IpcRouter {
 public:
  explicit IpcRouter(DesktopDatabase* database = nullptr,
                     const CredentialStore* credentials = nullptr);

  IpcReply invoke(const std::string& method, nlohmann::json params);
  [[nodiscard]] std::set<std::string> subscribers(const std::string& event) const;

  // The Agent→host request entry point the sidecar dispatches through. Exposed so
  // the host RPC surface can be exercised directly in tests. `cancel`, when set,
  // lets a long-running tool call observe a `host.tool.cancel` for its request.
  [[nodiscard]] nlohmann::json handle_host_request(
      const std::string& method, const nlohmann::json& params,
      const std::shared_ptr<std::atomic<bool>>& cancel = nullptr);

  // Delivers Agent sidecar notifications (for example `agent.started`) to the
  // WebView bridge. Set once at startup; safe to call from the sidecar thread.
  void set_event_sink(std::function<void(const std::string&, const nlohmann::json&)> sink);

 private:
  IpcReply subscribe(const nlohmann::json& params, bool add);
  IpcReply server_command(const std::string& method,
                          const nlohmann::json& params);
  IpcReply network_proxy_command(const std::string& method,
                                 nlohmann::json& params);
  IpcReply host_key_command(const std::string& method,
                            const nlohmann::json& params);
  IpcReply server_read_command(const std::string& method,
                               const nlohmann::json& params);
  IpcReply connection_command(const std::string& method,
                              const nlohmann::json& params);
  IpcReply terminal_command(const std::string& method,
                            const nlohmann::json& params);
  [[nodiscard]] std::string issue_probe_ticket(const std::string& server_id,
                                               const std::string& host, int port,
                                               const std::string& fingerprint);
  [[nodiscard]] std::optional<std::string> claim_probe_ticket(
      const std::string& ticket, const std::string& server_id, const std::string& host,
      int port, const std::string& fingerprint);
  IpcReply agent_command(const std::string& method, const nlohmann::json& params);
  IpcReply agent_run_command(const std::string& method, const nlohmann::json& params);
  IpcReply mcp_command(const std::string& method, const nlohmann::json& params);
  IpcReply provider_command(const std::string& method, const nlohmann::json& params);
  SidecarClient& ensure_sidecar();
  void configure_sidecar();

  mutable std::mutex mutex_;
  DesktopDatabase* database_;
  DesktopCredentialStore default_credentials_;
  const CredentialStore* credentials_;
  std::unordered_map<std::string, std::set<std::string>> subscriptions_;
  std::unique_ptr<SidecarClient> sidecar_;
  std::mutex sidecar_mutex_;
  std::unique_ptr<TerminalManager> terminals_;
  std::unique_ptr<McpSupervisor> mcp_;
  LocalPathHandles local_paths_;
  struct ProbeTicket {
    std::string server_id;
    std::string host;
    int port = 0;
    std::string fingerprint;
    std::chrono::steady_clock::time_point expires_at;
  };
  std::map<std::string, ProbeTicket> probe_tickets_;
  // Bounded in-memory replay cache for guarded host tool calls. The durable
  // ledger records only intent/terminal state; a response may be replayed only
  // while this process still holds it (matching the retired Rust host).
  std::mutex host_tool_replay_mutex_;
  std::map<std::pair<std::string, std::string>, nlohmann::json> host_tool_replays_;
  // In-flight OAuth authorizations, keyed by server id. `mcp_oauth_connect`
  // blocks while the browser redirect or device-code poll runs, so a token lets
  // `mcp_oauth_cancel` (on another thread) stop it.
  std::mutex oauth_flows_mutex_;
  std::map<std::string, std::shared_ptr<std::atomic<bool>>> oauth_flows_;
  std::function<void(const std::string&, const nlohmann::json&)> event_sink_;
};

[[nodiscard]] bool is_external_url(const std::string& url);
[[nodiscard]] bool open_external_url(const std::string& url);
[[nodiscard]] std::string operating_system();

}  // namespace yukinal::desktop_host

#pragma once

#include "yukinal/desktop_host/credential_store.hpp"
#include "yukinal/desktop_host/database.hpp"
#include "yukinal/desktop_host/local_paths.hpp"
#include "yukinal/desktop_host/sidecar.hpp"

#include <functional>
#include <memory>
#include <mutex>
#include <nlohmann/json.hpp>
#include <set>
#include <string>
#include <unordered_map>

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
  IpcReply agent_command(const std::string& method, const nlohmann::json& params);
  IpcReply agent_run_command(const std::string& method, const nlohmann::json& params);
  IpcReply mcp_command(const std::string& method, const nlohmann::json& params);
  IpcReply provider_command(const std::string& method, const nlohmann::json& params);
  [[nodiscard]] nlohmann::json handle_host_request(const std::string& method,
                                                   const nlohmann::json& params);
  SidecarClient& ensure_sidecar();
  void configure_sidecar();

  mutable std::mutex mutex_;
  DesktopDatabase* database_;
  DesktopCredentialStore default_credentials_;
  const CredentialStore* credentials_;
  std::unordered_map<std::string, std::set<std::string>> subscriptions_;
  std::unique_ptr<SidecarClient> sidecar_;
  std::mutex sidecar_mutex_;
  LocalPathHandles local_paths_;
  std::function<void(const std::string&, const nlohmann::json&)> event_sink_;
};

[[nodiscard]] bool is_external_url(const std::string& url);
[[nodiscard]] bool open_external_url(const std::string& url);
[[nodiscard]] std::string operating_system();

}  // namespace yukinal::desktop_host

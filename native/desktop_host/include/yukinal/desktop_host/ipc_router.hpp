#pragma once

#include "yukinal/desktop_host/credential_store.hpp"
#include "yukinal/desktop_host/database.hpp"

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

 private:
  IpcReply subscribe(const nlohmann::json& params, bool add);
  IpcReply server_command(const std::string& method,
                          const nlohmann::json& params);
  IpcReply network_proxy_command(const std::string& method,
                                 nlohmann::json& params);

  mutable std::mutex mutex_;
  DesktopDatabase* database_;
  DesktopCredentialStore default_credentials_;
  const CredentialStore* credentials_;
  std::unordered_map<std::string, std::set<std::string>> subscriptions_;
};

[[nodiscard]] bool is_external_url(const std::string& url);
[[nodiscard]] bool open_external_url(const std::string& url);
[[nodiscard]] std::string operating_system();

}  // namespace yukinal::desktop_host

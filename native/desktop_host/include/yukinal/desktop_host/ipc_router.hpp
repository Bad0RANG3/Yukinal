#pragma once

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
  IpcReply invoke(const std::string& method, const nlohmann::json& params);
  [[nodiscard]] std::set<std::string> subscribers(const std::string& event) const;

 private:
  IpcReply subscribe(const nlohmann::json& params, bool add);

  mutable std::mutex mutex_;
  std::unordered_map<std::string, std::set<std::string>> subscriptions_;
};

[[nodiscard]] bool is_external_url(const std::string& url);
[[nodiscard]] bool open_external_url(const std::string& url);
[[nodiscard]] std::string operating_system();

}  // namespace yukinal::desktop_host

#pragma once

#include "yukinal/desktop_host/database.hpp"
#include <functional>
#include <map>
#include <mutex>
#include <string>

namespace yukinal::desktop_host {

// The clock only admits asynchronous AI inspections. Resource/log feeds own their clocks.
class WatchScheduler {
 public:
  using Start = std::function<nlohmann::json(const nlohmann::json&)>;
  // true only after quiescence is confirmed, false while cancellation is pending/uncertain.
  using Stop = std::function<bool(const std::string&)>;
  using Notify = std::function<void(const nlohmann::json&)>;
  WatchScheduler(DesktopDatabase& database, Start start, Stop stop, Notify notify, Notify activity = {});
  // Called on the router's serialized lane, never the WebView thread.
  void tick(const std::string& now);
  // Called by the sidecar reader; no RPC or other blocking sidecar calls here.
  void on_event(const nlohmann::json& event, const std::string& now);

 private:
  struct Active {
    std::string schedule_id;
    std::string task_id;
    std::string deadline;
    std::string stopping;
    bool observed = false;
    std::string server_id;
    std::string goal;
  };
  void activity(const std::string& id, const Active& active, const std::string& stage,
                const std::string& now, const nlohmann::json& detail = nlohmann::json::object());
  void finish(const std::string& id, const std::string& status, const std::string& outcome,
              const std::string& summary, const std::optional<std::string>& error,
              const std::string& now);
  DesktopDatabase& database_;
  Start start_;
  Stop stop_;
  Notify notify_;
  Notify activity_;
  std::mutex mutex_;
  std::map<std::string, Active> active_;
  bool recovered_ = false;
};
}  // namespace yukinal::desktop_host

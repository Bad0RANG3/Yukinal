#pragma once

#include <filesystem>
#include <nlohmann/json.hpp>
#include <optional>
#include <sqlite3.h>
#include <string>

namespace yukinal::desktop_host {

class DesktopDatabase {
 public:
  explicit DesktopDatabase(const std::filesystem::path& data_directory);
  ~DesktopDatabase();
  DesktopDatabase(const DesktopDatabase&) = delete;
  DesktopDatabase& operator=(const DesktopDatabase&) = delete;

  [[nodiscard]] nlohmann::json list_servers() const;
  [[nodiscard]] nlohmann::json list_workspaces() const;
  [[nodiscard]] nlohmann::json list_activities(
      const std::optional<std::string>& server_id, int limit) const;

 private:
  sqlite3* connection_ = nullptr;
};

[[nodiscard]] std::filesystem::path default_data_directory();

}  // namespace yukinal::desktop_host

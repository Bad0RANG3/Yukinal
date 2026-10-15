#pragma once

#include <filesystem>
#include <nlohmann/json.hpp>
#include <sqlite3.h>

namespace yukinal::desktop_host {

class DesktopDatabase {
 public:
  explicit DesktopDatabase(const std::filesystem::path& data_directory);
  ~DesktopDatabase();
  DesktopDatabase(const DesktopDatabase&) = delete;
  DesktopDatabase& operator=(const DesktopDatabase&) = delete;

  [[nodiscard]] nlohmann::json list_servers() const;
  [[nodiscard]] nlohmann::json list_workspaces() const;

 private:
  sqlite3* connection_ = nullptr;
};

[[nodiscard]] std::filesystem::path default_data_directory();

}  // namespace yukinal::desktop_host

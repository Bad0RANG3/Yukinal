#pragma once

#include <filesystem>
#include <mutex>
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
  [[nodiscard]] nlohmann::json list_tool_executions(
      const std::optional<std::string>& trace_id,
      const std::optional<std::string>& server_id, int limit) const;
  [[nodiscard]] nlohmann::json list_chat_sessions(
      const std::optional<std::string>& query, const std::optional<bool>& archived,
      int limit, int offset) const;
  [[nodiscard]] nlohmann::json get_chat_session(const std::string& session_id) const;
  [[nodiscard]] nlohmann::json create_chat_session(
      const std::string& id, const std::optional<std::string>& workspace_id,
      const std::optional<std::string>& server_id, const std::string& title,
      const std::string& timestamp) const;
  [[nodiscard]] nlohmann::json append_chat_message(
      const std::string& id, const std::string& session_id, const std::string& role,
      const std::string& content, const std::optional<nlohmann::json>& parts,
      const std::optional<std::string>& trace_id, const std::string& timestamp) const;
  [[nodiscard]] nlohmann::json set_chat_session_archived(
      const std::string& session_id,
      const std::optional<std::string>& archived_at) const;
  [[nodiscard]] nlohmann::json rename_chat_session(
      const std::string& session_id, const std::string& title) const;
  [[nodiscard]] nlohmann::json delete_chat_session(const std::string& session_id) const;

 private:
  sqlite3* connection_ = nullptr;
  mutable std::recursive_mutex mutex_;
};

[[nodiscard]] std::filesystem::path default_data_directory();

}  // namespace yukinal::desktop_host

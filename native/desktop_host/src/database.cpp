#include "yukinal/desktop_host/database.hpp"

#include <cstdlib>
#include <cstdint>
#include <initializer_list>
#include <iterator>
#include <optional>
#include <set>
#include <stdexcept>
#include <string>
#include <utility>

#ifdef _WIN32
#include <windows.h>
#endif

namespace yukinal::desktop_host {
namespace {

std::string path_as_utf8(const std::filesystem::path& path) {
  const auto encoded = path.u8string();
  return {encoded.begin(), encoded.end()};
}

const char* column_text(sqlite3_stmt* statement, int column) {
  const auto* value = sqlite3_column_text(statement, column);
  return value == nullptr ? "" : reinterpret_cast<const char*>(value);
}

nlohmann::json parse_json_column(sqlite3_stmt* statement, int column,
                                 const char* description) {
  try {
    return nlohmann::json::parse(column_text(statement, column));
  } catch (const nlohmann::json::exception&) {
    throw std::runtime_error(std::string("Stored server ") + description + " is invalid JSON");
  }
}

std::optional<nlohmann::json> parse_optional_json_column(sqlite3_stmt* statement, int column,
                                                          const char* description) {
  if (sqlite3_column_type(statement, column) == SQLITE_NULL) return std::nullopt;
  const auto* text = column_text(statement, column);
  if (*text == '\0') return std::nullopt;
  try {
    auto value = nlohmann::json::parse(text);
    if (value.is_null()) return std::nullopt;
    return value;
  } catch (const nlohmann::json::exception&) {
    throw std::runtime_error(std::string("Stored server ") + description + " is invalid JSON");
  }
}

void add_optional_text(nlohmann::json& object, const char* key,
                       sqlite3_stmt* statement, int column) {
  if (sqlite3_column_type(statement, column) != SQLITE_NULL) {
    object[key] = column_text(statement, column);
  }
}

bool is_one_of(const std::string& value, std::initializer_list<const char*> options) {
  return std::any_of(options.begin(), options.end(), [&value](const char* option) {
    return value == option;
  });
}

std::filesystem::path environment_path(const char* name) {
#ifdef _WIN32
  wchar_t buffer[32768];
  const auto length = GetEnvironmentVariableW(
      std::filesystem::path(name).wstring().c_str(), buffer,
      static_cast<DWORD>(std::size(buffer)));
  if (length == 0 || length >= std::size(buffer)) return {};
  return std::filesystem::path(std::wstring(buffer, length));
#else
  const auto* value = std::getenv(name);
  if (value == nullptr || *value == '\0') return {};
  return std::filesystem::u8path(value);
#endif
}

class Statement {
 public:
  Statement(sqlite3* connection, const char* sql) : connection_(connection) {
    const auto result = sqlite3_prepare_v2(connection, sql, -1, &statement_, nullptr);
    if (result != SQLITE_OK) {
      throw std::runtime_error(sqlite3_errmsg(connection));
    }
  }
  ~Statement() { sqlite3_finalize(statement_); }
  Statement(const Statement&) = delete;
  Statement& operator=(const Statement&) = delete;

  sqlite3_stmt* get() const { return statement_; }

 private:
  sqlite3* connection_;
  sqlite3_stmt* statement_ = nullptr;
};

}  // namespace

DesktopDatabase::DesktopDatabase(const std::filesystem::path& data_directory) {
  if (!data_directory.is_absolute()) {
    throw std::runtime_error("The Yukinal data directory must be an absolute path");
  }
  std::error_code error;
  const auto file_path = data_directory / "yukinal.db";
  const bool database_exists = std::filesystem::exists(file_path, error);
  if (error) throw std::runtime_error("Could not inspect the Yukinal database path");
  if (!database_exists) return;

  const auto database_path = path_as_utf8(file_path);
  const auto result = sqlite3_open_v2(database_path.c_str(), &connection_,
                                      SQLITE_OPEN_READONLY | SQLITE_OPEN_FULLMUTEX,
                                      nullptr);
  if (result != SQLITE_OK) {
    const std::string message = connection_ == nullptr
                                    ? "Could not open the Yukinal database"
                                    : sqlite3_errmsg(connection_);
    if (connection_ != nullptr) sqlite3_close_v2(connection_);
    connection_ = nullptr;
    throw std::runtime_error(message);
  }
  sqlite3_busy_timeout(connection_, 5000);
  sqlite3_db_config(connection_, SQLITE_DBCONFIG_DEFENSIVE, 1, nullptr);
  char* error_message = nullptr;
  const auto pragma_result = sqlite3_exec(connection_, "PRAGMA trusted_schema=OFF;", nullptr,
                                          nullptr, &error_message);
  if (pragma_result != SQLITE_OK) {
    const std::string message = error_message == nullptr ? "Could not configure the database"
                                                          : error_message;
    sqlite3_free(error_message);
    sqlite3_close_v2(connection_);
    connection_ = nullptr;
    throw std::runtime_error(message);
  }
}

DesktopDatabase::~DesktopDatabase() {
  if (connection_ != nullptr) sqlite3_close_v2(connection_);
}

nlohmann::json DesktopDatabase::list_servers() const {
  if (connection_ == nullptr) return {{"servers", nlohmann::json::array()}};
  std::set<std::string> columns;
  {
    Statement metadata(connection_, "PRAGMA table_info(servers)");
    for (;;) {
      const auto result = sqlite3_step(metadata.get());
      if (result == SQLITE_DONE) break;
      if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
      columns.emplace(column_text(metadata.get(), 1));
    }
  }
  const auto select_optional = [&columns](const char* name) {
    return columns.contains(name) ? std::string(name) : std::string("NULL AS ") + name;
  };
  const auto sql = std::string(R"SQL(
    SELECT id, name, host, port, username, identity_id, group_id, capabilities,
           status, environment, region, hostname, os, tags, workspace_ids,
  )SQL") + select_optional("host_ca_public_key") + ", " +
                   select_optional("host_principals") + ", " +
                   select_optional("host_krl_path") + ", " +
                   select_optional("host_krl_url") + ", " +
                   select_optional("host_krl_signers") + R"SQL(,
           created_at, updated_at
      FROM servers
     ORDER BY name COLLATE NOCASE, id
  )SQL";
  Statement rows(connection_, sql.c_str());
  nlohmann::json servers = nlohmann::json::array();
  for (;;) {
    const auto result = sqlite3_step(rows.get());
    if (result == SQLITE_DONE) break;
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));

    const auto capabilities = parse_json_column(rows.get(), 7, "capabilities");
    if (!capabilities.is_object()) throw std::runtime_error("Stored server capabilities are not an object");

    nlohmann::json connection = {
        {"host", column_text(rows.get(), 2)},
        {"port", sqlite3_column_int(rows.get(), 3)},
        {"username", column_text(rows.get(), 4)},
    };
    if (sqlite3_column_type(rows.get(), 5) != SQLITE_NULL) {
      connection["identityId"] = column_text(rows.get(), 5);
    }

    if (sqlite3_column_type(rows.get(), 15) != SQLITE_NULL) {
      nlohmann::json authority = {
          {"caPublicKey", column_text(rows.get(), 15)},
          {"principals", parse_json_column(rows.get(), 16, "host principals")},
      };
      if (!authority["principals"].is_array()) {
        throw std::runtime_error("Stored host principals are not an array");
      }
      add_optional_text(authority, "revocationListPath", rows.get(), 17);
      add_optional_text(authority, "revocationListUrl", rows.get(), 18);
      if (const auto signers = parse_optional_json_column(rows.get(), 19, "host KRL signers")) {
        if (!signers->is_array()) throw std::runtime_error("Stored host KRL signers are not an array");
        if (!signers->empty()) authority["revocationListSigners"] = *signers;
      }
      connection["hostCertificateAuthority"] = std::move(authority);
    } else if (sqlite3_column_type(rows.get(), 16) != SQLITE_NULL ||
               sqlite3_column_type(rows.get(), 17) != SQLITE_NULL ||
               sqlite3_column_type(rows.get(), 18) != SQLITE_NULL ||
               sqlite3_column_type(rows.get(), 19) != SQLITE_NULL) {
      throw std::runtime_error("Stored host certificate authority is incomplete");
    }

    nlohmann::json server = {
        {"id", column_text(rows.get(), 0)},
        {"name", column_text(rows.get(), 1)},
        {"connection", std::move(connection)},
        {"capabilities", capabilities},
        {"status", column_text(rows.get(), 8)},
        {"metadata", {{"environment", column_text(rows.get(), 9)}}},
        {"createdAt", column_text(rows.get(), 20)},
        {"updatedAt", column_text(rows.get(), 21)},
    };
    if (sqlite3_column_type(rows.get(), 6) != SQLITE_NULL) {
      server["groupId"] = column_text(rows.get(), 6);
    }
    add_optional_text(server["metadata"], "region", rows.get(), 10);
    add_optional_text(server["metadata"], "hostname", rows.get(), 11);
    add_optional_text(server["metadata"], "os", rows.get(), 12);
    if (const auto tags = parse_optional_json_column(rows.get(), 13, "tags")) {
      if (!tags->is_array()) throw std::runtime_error("Stored server tags are not an array");
      server["metadata"]["tags"] = *tags;
    }
    if (const auto workspace_ids = parse_optional_json_column(rows.get(), 14, "workspace ids")) {
      if (!workspace_ids->is_array()) throw std::runtime_error("Stored workspace ids are not an array");
      server["metadata"]["workspaceIds"] = *workspace_ids;
    }
    servers.push_back(std::move(server));
  }
  return {{"servers", std::move(servers)}};
}

nlohmann::json DesktopDatabase::list_workspaces() const {
  if (connection_ == nullptr) return {{"workspaces", nlohmann::json::array()}};
  Statement rows(connection_, R"SQL(
    SELECT id, name, server_ids, repositories, provider_ids, default_environment
      FROM workspaces
     ORDER BY name COLLATE NOCASE, id
  )SQL");
  nlohmann::json workspaces = nlohmann::json::array();
  for (;;) {
    const auto result = sqlite3_step(rows.get());
    if (result == SQLITE_DONE) break;
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
    auto server_ids = parse_json_column(rows.get(), 2, "workspace server ids");
    auto repositories = parse_json_column(rows.get(), 3, "workspace repositories");
    auto provider_ids = parse_json_column(rows.get(), 4, "workspace provider ids");
    if (!server_ids.is_array() || !repositories.is_array() || !provider_ids.is_array()) {
      throw std::runtime_error("Stored workspace collections must be arrays");
    }
    workspaces.push_back({
        {"id", column_text(rows.get(), 0)},
        {"name", column_text(rows.get(), 1)},
        {"serverIds", std::move(server_ids)},
        {"repositories", std::move(repositories)},
        {"providerIds", std::move(provider_ids)},
        {"defaultEnvironment", column_text(rows.get(), 5)},
    });
  }
  return {{"workspaces", std::move(workspaces)}};
}

nlohmann::json DesktopDatabase::list_activities(
    const std::optional<std::string>& server_id, int limit) const {
  if (limit < 1 || limit > 100) {
    throw std::invalid_argument("Activity limit must be between 1 and 100");
  }
  if (connection_ == nullptr) return {{"activities", nlohmann::json::array()}};

  const char* sql = server_id.has_value()
      ? R"SQL(
          SELECT id, server_id, workspace_id, type, title, description, source, actor,
                 reason, outcome, trace_id, created_at
            FROM activities
           WHERE server_id = ?1
           ORDER BY created_at DESC
           LIMIT ?2
        )SQL"
      : R"SQL(
          SELECT id, server_id, workspace_id, type, title, description, source, actor,
                 reason, outcome, trace_id, created_at
            FROM activities
           ORDER BY created_at DESC
           LIMIT ?1
        )SQL";
  Statement rows(connection_, sql);
  int limit_parameter = 1;
  if (server_id.has_value()) {
    const auto bind_result = sqlite3_bind_text(rows.get(), 1, server_id->c_str(),
                                               static_cast<int>(server_id->size()),
                                               SQLITE_TRANSIENT);
    if (bind_result != SQLITE_OK) throw std::runtime_error(sqlite3_errmsg(connection_));
    limit_parameter = 2;
  }
  if (sqlite3_bind_int(rows.get(), limit_parameter, limit) != SQLITE_OK) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }

  nlohmann::json activities = nlohmann::json::array();
  for (;;) {
    const auto result = sqlite3_step(rows.get());
    if (result == SQLITE_DONE) break;
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));

    const std::string type = column_text(rows.get(), 3);
    const std::string source = column_text(rows.get(), 6);
    if (!is_one_of(type, {"connection", "authentication", "configuration", "deployment",
                          "service", "container", "file_change", "agent_action",
                          "approval", "health"}) ||
        !is_one_of(source, {"agent", "user", "system", "docker", "git", "cloud"})) {
      throw std::runtime_error("Stored activity has an unknown type or source");
    }

    nlohmann::json activity = {
        {"id", column_text(rows.get(), 0)},
        {"type", type},
        {"title", column_text(rows.get(), 4)},
        {"source", source},
        {"actor", column_text(rows.get(), 7)},
        {"createdAt", column_text(rows.get(), 11)},
    };
    add_optional_text(activity, "serverId", rows.get(), 1);
    add_optional_text(activity, "workspaceId", rows.get(), 2);
    add_optional_text(activity, "description", rows.get(), 5);
    add_optional_text(activity, "reason", rows.get(), 8);
    add_optional_text(activity, "traceId", rows.get(), 10);
    if (sqlite3_column_type(rows.get(), 9) != SQLITE_NULL) {
      const std::string outcome = column_text(rows.get(), 9);
      if (!is_one_of(outcome, {"success", "failure", "cancelled", "denied"})) {
        throw std::runtime_error("Stored activity has an unknown outcome");
      }
      activity["outcome"] = outcome;
    }
    activities.push_back(std::move(activity));
  }
  return {{"activities", std::move(activities)}};
}

nlohmann::json DesktopDatabase::list_tool_executions(
    const std::optional<std::string>& trace_id,
    const std::optional<std::string>& server_id, int limit) const {
  if (limit < 1 || limit > 100) {
    throw std::invalid_argument("Tool execution limit must be between 1 and 100");
  }
  if (connection_ == nullptr) return {{"executions", nlohmann::json::array()}};

  const char* sql = trace_id.has_value()
      ? R"SQL(
          SELECT trace_id, step_id, call_id, tool_name, server_id, environment, risk_level,
                 decision, approved_by, status, input, output, error, started_at, ended_at,
                 duration_ms
            FROM tool_executions
           WHERE trace_id = ?1
           ORDER BY started_at
           LIMIT ?2
        )SQL"
      : server_id.has_value()
          ? R"SQL(
              SELECT trace_id, step_id, call_id, tool_name, server_id, environment, risk_level,
                     decision, approved_by, status, input, output, error, started_at, ended_at,
                     duration_ms
                FROM tool_executions
               WHERE server_id = ?1
               ORDER BY started_at DESC
               LIMIT ?2
            )SQL"
          : R"SQL(
              SELECT trace_id, step_id, call_id, tool_name, server_id, environment, risk_level,
                     decision, approved_by, status, input, output, error, started_at, ended_at,
                     duration_ms
                FROM tool_executions
               ORDER BY started_at DESC
               LIMIT ?1
            )SQL";
  Statement rows(connection_, sql);
  int limit_parameter = 1;
  const auto& filter = trace_id.has_value() ? trace_id : server_id;
  if (filter.has_value()) {
    const auto bind_result = sqlite3_bind_text(rows.get(), 1, filter->c_str(),
                                               static_cast<int>(filter->size()),
                                               SQLITE_TRANSIENT);
    if (bind_result != SQLITE_OK) throw std::runtime_error(sqlite3_errmsg(connection_));
    limit_parameter = 2;
  }
  if (sqlite3_bind_int(rows.get(), limit_parameter, limit) != SQLITE_OK) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }

  nlohmann::json executions = nlohmann::json::array();
  for (;;) {
    const auto result = sqlite3_step(rows.get());
    if (result == SQLITE_DONE) break;
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));

    const std::string environment = column_text(rows.get(), 5);
    const std::string risk_level = column_text(rows.get(), 6);
    const std::string decision = column_text(rows.get(), 7);
    const std::string status = column_text(rows.get(), 9);
    if (!is_one_of(environment, {"local", "development", "staging", "production", "unknown"}) ||
        !is_one_of(risk_level, {"read", "low", "medium", "high", "critical"}) ||
        !is_one_of(decision, {"auto", "ask", "deny"}) ||
        !is_one_of(status, {"pending", "running", "waiting_approval", "success", "failed",
                            "cancelled"})) {
      throw std::runtime_error("Stored tool execution has an unknown enum value");
    }

    nlohmann::json input;
    try {
      input = nlohmann::json::parse(column_text(rows.get(), 10));
    } catch (const nlohmann::json::exception&) {
      throw std::runtime_error("Stored tool execution input is invalid JSON");
    }
    nlohmann::json execution = {
        {"traceId", column_text(rows.get(), 0)},
        {"stepId", column_text(rows.get(), 1)},
        {"callId", column_text(rows.get(), 2)},
        {"toolName", column_text(rows.get(), 3)},
        {"environment", environment},
        {"riskLevel", risk_level},
        {"decision", decision},
        {"status", status},
        {"input", std::move(input)},
        {"startedAt", column_text(rows.get(), 13)},
    };
    add_optional_text(execution, "serverId", rows.get(), 4);
    add_optional_text(execution, "approvedBy", rows.get(), 8);
    add_optional_text(execution, "error", rows.get(), 12);
    add_optional_text(execution, "endedAt", rows.get(), 14);
    if (sqlite3_column_type(rows.get(), 8) != SQLITE_NULL) {
      const std::string approved_by = column_text(rows.get(), 8);
      if (!is_one_of(approved_by, {"user", "policy"})) {
        throw std::runtime_error("Stored tool execution has an unknown approval source");
      }
    }
    if (sqlite3_column_type(rows.get(), 11) != SQLITE_NULL) {
      try {
        execution["output"] = nlohmann::json::parse(column_text(rows.get(), 11));
      } catch (const nlohmann::json::exception&) {
        throw std::runtime_error("Stored tool execution output is invalid JSON");
      }
    }
    if (sqlite3_column_type(rows.get(), 15) != SQLITE_NULL) {
      const auto duration = sqlite3_column_int64(rows.get(), 15);
      if (duration < 0) throw std::runtime_error("Stored tool execution duration is invalid");
      execution["durationMs"] = static_cast<std::uint64_t>(duration);
    }
    executions.push_back(std::move(execution));
  }
  return {{"executions", std::move(executions)}};
}

std::filesystem::path default_data_directory() {
  if (const auto override = environment_path("YUKINAL_DATA_DIR"); !override.empty()) {
    return override;
  }
#ifdef _WIN32
  auto base = environment_path("APPDATA");
  if (base.empty()) base = environment_path("USERPROFILE") / "AppData" / "Roaming";
  return base / "dev.yukinal.workspace";
#elif defined(__APPLE__)
  return environment_path("HOME") / "Library" / "Application Support" / "dev.yukinal.workspace";
#else
  auto base = environment_path("XDG_DATA_HOME");
  if (base.empty()) base = environment_path("HOME") / ".local" / "share";
  return base / "dev.yukinal.workspace";
#endif
}

}  // namespace yukinal::desktop_host

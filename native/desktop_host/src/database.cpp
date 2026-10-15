#include "yukinal/desktop_host/database.hpp"

#include <algorithm>
#include <cctype>
#include <cstdlib>
#include <cstdint>
#include <initializer_list>
#include <iterator>
#include <limits>
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

void bind_text(sqlite3* connection, sqlite3_stmt* statement, int index,
               const std::string& value) {
  if (value.size() > static_cast<size_t>(std::numeric_limits<int>::max())) {
    throw std::runtime_error("A local database text value exceeds SQLite's size limit");
  }
  if (sqlite3_bind_text(statement, index, value.data(), static_cast<int>(value.size()),
                        SQLITE_TRANSIENT) != SQLITE_OK) {
    throw std::runtime_error(sqlite3_errmsg(connection));
  }
}

void bind_optional_text(sqlite3* connection, sqlite3_stmt* statement, int index,
                        const std::optional<std::string>& value) {
  if (!value.has_value()) {
    if (sqlite3_bind_null(statement, index) != SQLITE_OK) {
      throw std::runtime_error(sqlite3_errmsg(connection));
    }
    return;
  }
  bind_text(connection, statement, index, *value);
}

void execute_sql(sqlite3* connection, const char* sql) {
  char* error = nullptr;
  if (sqlite3_exec(connection, sql, nullptr, nullptr, &error) == SQLITE_OK) return;
  const std::string message = error == nullptr ? sqlite3_errmsg(connection) : error;
  sqlite3_free(error);
  throw std::runtime_error(message);
}

class Transaction {
 public:
  explicit Transaction(sqlite3* connection) : connection_(connection) {
    execute_sql(connection_, "BEGIN IMMEDIATE;");
  }
  ~Transaction() {
    if (active_) sqlite3_exec(connection_, "ROLLBACK;", nullptr, nullptr, nullptr);
  }
  Transaction(const Transaction&) = delete;
  Transaction& operator=(const Transaction&) = delete;

  void commit() {
    execute_sql(connection_, "COMMIT;");
    active_ = false;
  }

 private:
  sqlite3* connection_;
  bool active_ = true;
};

constexpr const char* kChatSessionSelect = R"SQL(
  SELECT s.id, s.workspace_id, s.server_id, s.title, s.created_at, s.updated_at,
         s.archived_at, COUNT(m.id),
         (SELECT substr(last_message.content, 1, 240)
            FROM chat_messages last_message
           WHERE last_message.session_id = s.id
           ORDER BY last_message.created_at DESC, last_message.id DESC
           LIMIT 1)
    FROM chat_sessions s
    LEFT JOIN chat_messages m ON m.session_id = s.id
)SQL";

std::string normalized_preview(std::string value) {
  std::string preview;
  preview.reserve(value.size());
  bool pending_space = false;
  for (const auto character : value) {
    if (std::isspace(static_cast<unsigned char>(character))) {
      pending_space = !preview.empty();
      continue;
    }
    if (pending_space) preview.push_back(' ');
    pending_space = false;
    preview.push_back(character);
  }
  return preview;
}

nlohmann::json chat_session_from_row(sqlite3* connection, sqlite3_stmt* row) {
  const auto message_count = sqlite3_column_int64(row, 7);
  if (message_count < 0 ||
      static_cast<std::uint64_t>(message_count) > std::numeric_limits<std::uint32_t>::max()) {
    throw std::runtime_error("Stored chat message count is out of range");
  }
  nlohmann::json session = {
      {"id", column_text(row, 0)},
      {"title", column_text(row, 3)},
      {"createdAt", column_text(row, 4)},
      {"updatedAt", column_text(row, 5)},
      {"messageCount", static_cast<std::uint32_t>(message_count)},
  };
  add_optional_text(session, "workspaceId", row, 1);
  add_optional_text(session, "serverId", row, 2);
  add_optional_text(session, "archivedAt", row, 6);
  if (sqlite3_column_type(row, 8) != SQLITE_NULL) {
    session["lastMessagePreview"] = normalized_preview(column_text(row, 8));
  }
  (void)connection;
  return session;
}

nlohmann::json chat_message_from_row(sqlite3* connection, sqlite3_stmt* row) {
  const std::string role = column_text(row, 2);
  if (!is_one_of(role, {"user", "assistant", "tool", "system"})) {
    throw std::runtime_error("Stored chat message has an unknown role");
  }
  nlohmann::json message = {
      {"id", column_text(row, 0)},
      {"sessionId", column_text(row, 1)},
      {"role", role},
      {"content", column_text(row, 3)},
      {"createdAt", column_text(row, 6)},
  };
  if (sqlite3_column_type(row, 4) != SQLITE_NULL) {
    try {
      auto parts = nlohmann::json::parse(column_text(row, 4));
      if (!parts.is_array()) throw std::runtime_error("Stored chat prompt parts are not an array");
      message["parts"] = std::move(parts);
    } catch (const nlohmann::json::exception&) {
      throw std::runtime_error("Stored chat prompt parts are invalid JSON");
    }
  }
  add_optional_text(message, "traceId", row, 5);
  (void)connection;
  return message;
}

nlohmann::json query_chat_session(sqlite3* connection, const std::string& session_id) {
  const std::string session_sql = std::string(kChatSessionSelect) +
      " WHERE s.id = ?1 GROUP BY s.id";
  Statement session_query(connection, session_sql.c_str());
  bind_text(connection, session_query.get(), 1, session_id);
  const auto session_result = sqlite3_step(session_query.get());
  if (session_result == SQLITE_DONE) throw std::runtime_error("Chat session was not found");
  if (session_result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection));
  auto session = chat_session_from_row(connection, session_query.get());

  Statement message_query(connection, R"SQL(
    SELECT id, session_id, role, content, parts_json, trace_id, created_at
      FROM chat_messages
     WHERE session_id = ?1
     ORDER BY created_at ASC, id ASC
  )SQL");
  bind_text(connection, message_query.get(), 1, session_id);
  nlohmann::json messages = nlohmann::json::array();
  for (;;) {
    const auto result = sqlite3_step(message_query.get());
    if (result == SQLITE_DONE) break;
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection));
    messages.push_back(chat_message_from_row(connection, message_query.get()));
  }
  return {{"session", std::move(session)}, {"messages", std::move(messages)}};
}

std::string escape_like(const std::string& value) {
  std::string escaped;
  escaped.reserve(value.size());
  for (const char character : value) {
    if (character == '\\' || character == '%' || character == '_') escaped.push_back('\\');
    escaped.push_back(character);
  }
  return escaped;
}

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
                                      SQLITE_OPEN_READWRITE | SQLITE_OPEN_FULLMUTEX,
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
  char* foreign_key_error = nullptr;
  if (sqlite3_exec(connection_, "PRAGMA foreign_keys=ON;", nullptr, nullptr,
                   &foreign_key_error) != SQLITE_OK) {
    const std::string message = foreign_key_error == nullptr
                                    ? "Could not enable database referential integrity"
                                    : foreign_key_error;
    sqlite3_free(foreign_key_error);
    sqlite3_close_v2(connection_);
    connection_ = nullptr;
    throw std::runtime_error(message);
  }
}

DesktopDatabase::~DesktopDatabase() {
  if (connection_ != nullptr) sqlite3_close_v2(connection_);
}

nlohmann::json DesktopDatabase::list_servers() const {
  std::lock_guard lock(mutex_);
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
  std::lock_guard lock(mutex_);
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
  std::lock_guard lock(mutex_);
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
  std::lock_guard lock(mutex_);
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

nlohmann::json DesktopDatabase::list_chat_sessions(
    const std::optional<std::string>& query, const std::optional<bool>& archived,
    int limit, int offset) const {
  std::lock_guard lock(mutex_);
  if (limit < 1 || limit > 100 || offset < 0 || offset > 10'000) {
    throw std::invalid_argument("Chat session paging is out of range");
  }
  if (connection_ == nullptr) {
    return {{"sessions", nlohmann::json::array()},
            {"counts", {{"active", 0}, {"archived", 0}}}};
  }

  std::string pattern = "%";
  pattern += escape_like(query.value_or(""));
  pattern += "%";
  std::string archive_clause;
  if (archived == true) archive_clause = "AND s.archived_at IS NOT NULL";
  else if (archived == false) archive_clause = "AND s.archived_at IS NULL";

  const std::string list_sql = std::string(kChatSessionSelect) + R"SQL(
     WHERE (s.title LIKE ?1 ESCAPE '\'
        OR EXISTS (
          SELECT 1 FROM chat_messages search_messages
           WHERE search_messages.session_id = s.id
             AND search_messages.content LIKE ?1 ESCAPE '\'
        ))
  )SQL" + archive_clause + R"SQL(
     GROUP BY s.id
     ORDER BY s.updated_at DESC, s.id DESC
     LIMIT ?2 OFFSET ?3
  )SQL";
  Statement rows(connection_, list_sql.c_str());
  bind_text(connection_, rows.get(), 1, pattern);
  if (sqlite3_bind_int(rows.get(), 2, limit) != SQLITE_OK ||
      sqlite3_bind_int(rows.get(), 3, offset) != SQLITE_OK) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  nlohmann::json sessions = nlohmann::json::array();
  for (;;) {
    const auto result = sqlite3_step(rows.get());
    if (result == SQLITE_DONE) break;
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
    sessions.push_back(chat_session_from_row(connection_, rows.get()));
  }

  Statement count_query(connection_, R"SQL(
    SELECT COALESCE(SUM(CASE WHEN s.archived_at IS NULL THEN 1 ELSE 0 END), 0),
           COALESCE(SUM(CASE WHEN s.archived_at IS NOT NULL THEN 1 ELSE 0 END), 0)
      FROM chat_sessions s
     WHERE (s.title LIKE ?1 ESCAPE '\'
        OR EXISTS (
          SELECT 1 FROM chat_messages search_messages
           WHERE search_messages.session_id = s.id
             AND search_messages.content LIKE ?1 ESCAPE '\'
        ))
  )SQL");
  bind_text(connection_, count_query.get(), 1, pattern);
  if (sqlite3_step(count_query.get()) != SQLITE_ROW) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  const auto active = sqlite3_column_int64(count_query.get(), 0);
  const auto archived_count = sqlite3_column_int64(count_query.get(), 1);
  if (active < 0 || archived_count < 0 ||
      static_cast<std::uint64_t>(active) > std::numeric_limits<std::uint32_t>::max() ||
      static_cast<std::uint64_t>(archived_count) > std::numeric_limits<std::uint32_t>::max()) {
    throw std::runtime_error("Stored chat session count is out of range");
  }
  return {{"sessions", std::move(sessions)},
          {"counts", {{"active", static_cast<std::uint32_t>(active)},
                       {"archived", static_cast<std::uint32_t>(archived_count)}}}};
}

nlohmann::json DesktopDatabase::get_chat_session(const std::string& session_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local chat database is unavailable");
  return query_chat_session(connection_, session_id);
}

nlohmann::json DesktopDatabase::create_chat_session(
    const std::string& id, const std::optional<std::string>& workspace_id,
    const std::optional<std::string>& server_id, const std::string& title,
    const std::string& timestamp) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local chat database is unavailable");
  Transaction transaction(connection_);
  Statement insert(connection_, R"SQL(
    INSERT INTO chat_sessions (
      id, workspace_id, server_id, title, created_at, updated_at, archived_at
    ) VALUES (?1, ?2, ?3, ?4, ?5, ?5, NULL)
  )SQL");
  bind_text(connection_, insert.get(), 1, id);
  bind_optional_text(connection_, insert.get(), 2, workspace_id);
  bind_optional_text(connection_, insert.get(), 3, server_id);
  bind_text(connection_, insert.get(), 4, title);
  bind_text(connection_, insert.get(), 5, timestamp);
  if (sqlite3_step(insert.get()) != SQLITE_DONE) throw std::runtime_error(sqlite3_errmsg(connection_));
  transaction.commit();
  const auto detail = query_chat_session(connection_, id);
  return {{"session", detail.at("session")}};
}

nlohmann::json DesktopDatabase::append_chat_message(
    const std::string& id, const std::string& session_id, const std::string& role,
    const std::string& content, const std::optional<nlohmann::json>& parts,
    const std::optional<std::string>& trace_id, const std::string& timestamp) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local chat database is unavailable");
  if (!is_one_of(role, {"user", "assistant", "tool", "system"})) {
    throw std::invalid_argument("Chat message role is invalid");
  }
  std::optional<std::string> encoded_parts;
  if (parts.has_value()) {
    if (!parts->is_array() || parts->empty() || parts->size() > 25) {
      throw std::invalid_argument("Chat prompt parts are out of range");
    }
    encoded_parts = parts->dump();
    if (encoded_parts->size() > 24 * 1024 * 1024) {
      throw std::invalid_argument("Chat prompt parts exceed the frame limit");
    }
  }

  Transaction transaction(connection_);
  Statement insert(connection_, R"SQL(
    INSERT INTO chat_messages (
      id, session_id, role, content, parts_json, trace_id, created_at
    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
  )SQL");
  bind_text(connection_, insert.get(), 1, id);
  bind_text(connection_, insert.get(), 2, session_id);
  bind_text(connection_, insert.get(), 3, role);
  bind_text(connection_, insert.get(), 4, content);
  bind_optional_text(connection_, insert.get(), 5, encoded_parts);
  bind_optional_text(connection_, insert.get(), 6, trace_id);
  bind_text(connection_, insert.get(), 7, timestamp);
  if (sqlite3_step(insert.get()) != SQLITE_DONE) throw std::runtime_error(sqlite3_errmsg(connection_));

  Statement update(connection_, "UPDATE chat_sessions SET updated_at = ?2 WHERE id = ?1");
  bind_text(connection_, update.get(), 1, session_id);
  bind_text(connection_, update.get(), 2, timestamp);
  if (sqlite3_step(update.get()) != SQLITE_DONE) throw std::runtime_error(sqlite3_errmsg(connection_));
  if (sqlite3_changes(connection_) == 0) throw std::runtime_error("Chat session was not found");
  transaction.commit();

  nlohmann::json message = {
      {"id", id}, {"sessionId", session_id}, {"role", role}, {"content", content},
      {"createdAt", timestamp},
  };
  if (parts.has_value()) message["parts"] = *parts;
  if (trace_id.has_value()) message["traceId"] = *trace_id;
  return {{"message", std::move(message)}};
}

nlohmann::json DesktopDatabase::set_chat_session_archived(
    const std::string& session_id,
    const std::optional<std::string>& archived_at) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local chat database is unavailable");
  Transaction transaction(connection_);
  Statement update(connection_, "UPDATE chat_sessions SET archived_at = ?2 WHERE id = ?1");
  bind_text(connection_, update.get(), 1, session_id);
  bind_optional_text(connection_, update.get(), 2, archived_at);
  if (sqlite3_step(update.get()) != SQLITE_DONE) throw std::runtime_error(sqlite3_errmsg(connection_));
  if (sqlite3_changes(connection_) == 0) throw std::runtime_error("Chat session was not found");
  transaction.commit();
  return {{"session", query_chat_session(connection_, session_id).at("session")}};
}

nlohmann::json DesktopDatabase::rename_chat_session(
    const std::string& session_id, const std::string& title) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local chat database is unavailable");
  Transaction transaction(connection_);
  Statement update(connection_, "UPDATE chat_sessions SET title = ?2 WHERE id = ?1");
  bind_text(connection_, update.get(), 1, session_id);
  bind_text(connection_, update.get(), 2, title);
  if (sqlite3_step(update.get()) != SQLITE_DONE) throw std::runtime_error(sqlite3_errmsg(connection_));
  if (sqlite3_changes(connection_) == 0) throw std::runtime_error("Chat session was not found");
  transaction.commit();
  return {{"session", query_chat_session(connection_, session_id).at("session")}};
}

nlohmann::json DesktopDatabase::delete_chat_session(const std::string& session_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local chat database is unavailable");
  Transaction transaction(connection_);
  Statement remove(connection_, "DELETE FROM chat_sessions WHERE id = ?1");
  bind_text(connection_, remove.get(), 1, session_id);
  if (sqlite3_step(remove.get()) != SQLITE_DONE) throw std::runtime_error(sqlite3_errmsg(connection_));
  if (sqlite3_changes(connection_) == 0) throw std::runtime_error("Chat session was not found");
  transaction.commit();
  return {{"deleted", true}};
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

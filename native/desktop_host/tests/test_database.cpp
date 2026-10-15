#include "yukinal/desktop_host/database.hpp"
#include "yukinal/desktop_host/ipc_router.hpp"

#include <chrono>
#include <cstdlib>
#include <filesystem>
#include <iostream>
#include <string>

namespace {

void check(bool condition, const char* message) {
  if (condition) return;
  std::cerr << message << '\n';
  std::exit(1);
}

struct TemporaryDatabaseDirectory {
  std::filesystem::path path = std::filesystem::temp_directory_path() /
      ("yukinal-desktop-host-" + std::to_string(
          std::chrono::steady_clock::now().time_since_epoch().count()));

  ~TemporaryDatabaseDirectory() {
    std::error_code error;
    std::filesystem::remove(path / "yukinal.db", error);
    std::filesystem::remove(path, error);
  }
};

void execute(sqlite3* database, const char* sql) {
  char* error = nullptr;
  if (sqlite3_exec(database, sql, nullptr, nullptr, &error) == SQLITE_OK) return;
  const std::string message = error == nullptr ? sqlite3_errmsg(database) : error;
  sqlite3_free(error);
  std::cerr << message << '\n';
  std::exit(1);
}

void seed_legacy_database(const std::filesystem::path& path) {
  std::filesystem::create_directories(path);
  sqlite3* database = nullptr;
  check(sqlite3_open((path / "yukinal.db").string().c_str(), &database) == SQLITE_OK,
        "could not create the temporary SQLite database");
  execute(database, R"SQL(
    CREATE TABLE servers (
      id TEXT PRIMARY KEY, name TEXT NOT NULL, host TEXT NOT NULL,
      port INTEGER NOT NULL, username TEXT NOT NULL, identity_id TEXT,
      group_id TEXT, capabilities TEXT NOT NULL, status TEXT NOT NULL,
      environment TEXT NOT NULL, region TEXT, hostname TEXT, os TEXT,
      tags TEXT, workspace_ids TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
    );
    CREATE TABLE workspaces (
      id TEXT PRIMARY KEY, name TEXT NOT NULL, server_ids TEXT NOT NULL,
      repositories TEXT NOT NULL, provider_ids TEXT NOT NULL,
      default_environment TEXT NOT NULL
    );
    CREATE TABLE activities (
      id TEXT PRIMARY KEY, server_id TEXT, workspace_id TEXT, type TEXT NOT NULL,
      title TEXT NOT NULL, description TEXT, source TEXT NOT NULL, actor TEXT NOT NULL,
      reason TEXT, outcome TEXT, trace_id TEXT, created_at TEXT NOT NULL
    );
    CREATE TABLE tool_executions (
      trace_id TEXT NOT NULL, step_id TEXT NOT NULL, call_id TEXT NOT NULL,
      tool_name TEXT NOT NULL, server_id TEXT, environment TEXT NOT NULL,
      risk_level TEXT NOT NULL, decision TEXT NOT NULL, approved_by TEXT,
      status TEXT NOT NULL, input TEXT NOT NULL, output TEXT, error TEXT,
      started_at TEXT NOT NULL, ended_at TEXT, duration_ms INTEGER
    );
    INSERT INTO servers VALUES (
      'srv_01abc', 'Staging', 'ssh.example.test', 2222, 'operator', NULL,
      'group_a', '{"linux":true,"docker":false}', 'disconnected', 'staging',
      NULL, 'stage-01', 'Linux', '["blue"]', '["ws_01"]',
      '2026-01-01T00:00:00Z', '2026-01-02T00:00:00Z'
    );
    INSERT INTO workspaces VALUES (
      'ws_01', 'Checkout', '["srv_01abc"]',
      '[{"id":"repo_01","name":"api","host":"remote","path":"/srv/api","serverId":"srv_01abc","gitUrl":"https://example.test/api.git","defaultBranch":"main"}]',
      '["provider_01"]', 'staging'
    );
    INSERT INTO activities VALUES (
      'act_01', 'srv_01abc', NULL, 'connection', 'Connected to server', NULL,
      'user', 'user', 'User requested connection', 'success', NULL, '2026-01-01T00:00:00.000Z'
    );
    INSERT INTO activities VALUES (
      'act_02', NULL, NULL, 'configuration', 'Provider updated', 'Updated default model',
      'system', 'core', NULL, 'success', NULL, '2026-01-01T00:01:00.000Z'
    );
    INSERT INTO tool_executions VALUES (
      'trc_01', 'step_01', 'call_01', 'ssh.host_snapshot', 'srv_01abc',
      'staging', 'read', 'auto', NULL, 'success', '{"target":"all"}',
      '{"healthy":true}', NULL, '2026-01-01T00:02:00.000Z',
      '2026-01-01T00:02:01.000Z', 17
    );
    INSERT INTO tool_executions VALUES (
      'trc_02', 'step_02', 'call_02', 'service.status', NULL,
      'unknown', 'low', 'ask', NULL, 'waiting_approval', '{"service":"nginx"}',
      NULL, NULL, '2026-01-01T00:03:00.000Z', NULL, NULL
    );
    PRAGMA user_version=8;
  )SQL");
  sqlite3_close(database);
}

int query_integer(sqlite3* database, const char* sql) {
  sqlite3_stmt* statement = nullptr;
  check(sqlite3_prepare_v2(database, sql, -1, &statement, nullptr) == SQLITE_OK,
        "could not prepare the database compatibility query");
  check(sqlite3_step(statement) == SQLITE_ROW, "database compatibility query returned no row");
  const int value = sqlite3_column_int(statement, 0);
  sqlite3_finalize(statement);
  return value;
}

}  // namespace

int main() {
  using yukinal::desktop_host::DesktopDatabase;

  TemporaryDatabaseDirectory temporary;
  seed_legacy_database(temporary.path);
  {
    DesktopDatabase database(temporary.path);
    const nlohmann::json expected_servers = {{"servers", nlohmann::json::array({
      {{"id", "srv_01abc"}, {"name", "Staging"},
       {"connection", {{"host", "ssh.example.test"}, {"port", 2222},
                        {"username", "operator"}}},
       {"groupId", "group_a"}, {"capabilities", {{"linux", true}, {"docker", false}}},
       {"status", "disconnected"},
       {"metadata", {{"environment", "staging"}, {"hostname", "stage-01"},
                      {"os", "Linux"}, {"tags", nlohmann::json::array({"blue"})},
                      {"workspaceIds", nlohmann::json::array({"ws_01"})}}},
       {"createdAt", "2026-01-01T00:00:00Z"},
       {"updatedAt", "2026-01-02T00:00:00Z"}}
    })}};
    check(database.list_servers() == expected_servers,
          "server_list should preserve the shared server JSON contract");

    const nlohmann::json expected_workspaces = {{"workspaces", nlohmann::json::array({
      {{"id", "ws_01"}, {"name", "Checkout"},
       {"serverIds", nlohmann::json::array({"srv_01abc"})},
       {"repositories", nlohmann::json::array({
         {{"id", "repo_01"}, {"name", "api"}, {"host", "remote"},
          {"path", "/srv/api"}, {"serverId", "srv_01abc"},
          {"gitUrl", "https://example.test/api.git"}, {"defaultBranch", "main"}}
       })},
       {"providerIds", nlohmann::json::array({"provider_01"})},
       {"defaultEnvironment", "staging"}}
    })}};
    check(database.list_workspaces() == expected_workspaces,
          "workspace_list should preserve nested repository data");

    const nlohmann::json expected_activities = {{"activities", nlohmann::json::array({
      {{"id", "act_02"}, {"type", "configuration"}, {"title", "Provider updated"},
       {"source", "system"}, {"actor", "core"}, {"description", "Updated default model"},
       {"outcome", "success"}, {"createdAt", "2026-01-01T00:01:00.000Z"}},
      {{"id", "act_01"}, {"serverId", "srv_01abc"}, {"type", "connection"},
       {"title", "Connected to server"}, {"source", "user"}, {"actor", "user"},
       {"reason", "User requested connection"}, {"outcome", "success"},
       {"createdAt", "2026-01-01T00:00:00.000Z"}}
    })}};
    check(database.list_activities(std::nullopt, 50) == expected_activities,
          "activity_list should preserve the shared activity JSON contract and sort newest first");
    check(database.list_activities(std::string("srv_01abc"), 50) ==
              nlohmann::json{{"activities", nlohmann::json::array({
                expected_activities["activities"][1]
              })}},
          "activity_list should filter by server id");
    check(database.list_activities(std::nullopt, 1) ==
              nlohmann::json{{"activities", nlohmann::json::array({
                expected_activities["activities"][0]
              })}},
          "activity_list should honor its bounded limit");

    yukinal::desktop_host::IpcRouter router(&database);
    const auto response = router.invoke("activity_list", {{"limit", 2}});
    check(response.ok && response.value == expected_activities,
          "the C++ IPC router should expose the activity database response");
    check(!router.invoke("activity_list", {{"serverId", "srv_01abc"}, {"extra", true}}).ok,
          "activity_list should reject unknown parameters");

    const nlohmann::json expected_executions = {{"executions", nlohmann::json::array({
      {{"traceId", "trc_02"}, {"stepId", "step_02"}, {"callId", "call_02"},
       {"toolName", "service.status"}, {"environment", "unknown"}, {"riskLevel", "low"},
       {"decision", "ask"}, {"status", "waiting_approval"},
       {"input", {{"service", "nginx"}}}, {"startedAt", "2026-01-01T00:03:00.000Z"}},
      {{"traceId", "trc_01"}, {"stepId", "step_01"}, {"callId", "call_01"},
       {"toolName", "ssh.host_snapshot"}, {"serverId", "srv_01abc"},
       {"environment", "staging"}, {"riskLevel", "read"}, {"decision", "auto"},
       {"status", "success"}, {"input", {{"target", "all"}}},
       {"output", {{"healthy", true}}}, {"startedAt", "2026-01-01T00:02:00.000Z"},
       {"endedAt", "2026-01-01T00:02:01.000Z"}, {"durationMs", 17}}
    })}};
    check(database.list_tool_executions(std::nullopt, std::nullopt, 50) == expected_executions,
          "tool_execution_list should preserve the record contract and sort newest first");
    const nlohmann::json expected_trace = {{"executions", nlohmann::json::array({
      expected_executions["executions"][1]
    })}};
    check(database.list_tool_executions(std::string("trc_01"), std::string("srv_other"), 50) ==
              expected_trace,
          "trace filtering should take precedence over server filtering");
    check(database.list_tool_executions(std::nullopt, std::string("srv_01abc"), 50) ==
              expected_trace,
          "tool_execution_list should filter by server id");
    check(database.list_tool_executions(std::nullopt, std::nullopt, 1) ==
              nlohmann::json{{"executions", nlohmann::json::array({
                expected_executions["executions"][0]
              })}},
          "tool_execution_list should honor its bounded limit");
    const auto executions_response = router.invoke("tool_execution_list",
        {{"traceId", "trc_01"}, {"serverId", "srv_other"}, {"limit", 5}});
    check(executions_response.ok && executions_response.value == expected_trace,
          "the C++ IPC router should expose tool execution records");
  }

  sqlite3* readback = nullptr;
  const auto file = (temporary.path / "yukinal.db").string();
  check(sqlite3_open_v2(file.c_str(), &readback, SQLITE_OPEN_READONLY, nullptr) == SQLITE_OK,
        "could not reopen the temporary database read-only");
  check(query_integer(readback, "PRAGMA user_version") == 8,
        "the C++ reader must not change the legacy migration version");
  check(query_integer(readback, "SELECT count(*) FROM pragma_table_info('servers')") == 17,
        "the C++ reader must not add columns to the legacy schema");
  sqlite3_close(readback);

  TemporaryDatabaseDirectory absent;
  {
    DesktopDatabase database(absent.path);
    check(database.list_servers() == nlohmann::json{{"servers", nlohmann::json::array()}},
          "an absent database should return an empty server list");
    check(database.list_workspaces() == nlohmann::json{{"workspaces", nlohmann::json::array()}},
          "an absent database should return an empty workspace list");
  }
  check(!std::filesystem::exists(absent.path / "yukinal.db"),
        "reading an absent database must not create a new database file");
  return 0;
}

#include "yukinal/desktop_host/database.hpp"
#include "yukinal/desktop_host/ipc_router.hpp"
#include "schema_migrations.hpp"

#include <chrono>
#include <cstddef>
#include <cstdlib>
#include <filesystem>
#include <iostream>
#include <map>
#include <string>
#include <string_view>

namespace {

void check(bool condition, const char* message) {
  if (condition) return;
  std::cerr << message << '\n';
  std::exit(1);
}

// A hermetic credential store so the network proxy and server commands can be
// exercised without writing to the operating system credential store.
class MemoryCredentialStore : public yukinal::desktop_host::CredentialStore {
 public:
  using CredentialReference = yukinal::desktop_host::CredentialReference;
  using Secret = yukinal::desktop_host::Secret;

  CredentialReference set(const std::string& service, const std::string& account,
                          const Secret& secret) const override {
    CredentialReference reference{service, account};
    std::string value;
    if (secret.size() != 0) {
      value.assign(reinterpret_cast<const char*>(secret.data()),
                   reinterpret_cast<const char*>(secret.data()) + secret.size());
    }
    secrets_[reference.to_string()] = std::move(value);
    return reference;
  }
  Secret get(const CredentialReference& reference) const override {
    const auto found = secrets_.find(reference.to_string());
    if (found == secrets_.end()) {
      throw yukinal::desktop_host::CredentialNotFoundError("Credential was not found");
    }
    return Secret{std::string_view(found->second)};
  }
  bool has(const CredentialReference& reference) const override {
    return secrets_.count(reference.to_string()) != 0;
  }
  void erase(const CredentialReference& reference) const override {
    secrets_.erase(reference.to_string());
  }
  [[nodiscard]] std::size_t size() const { return secrets_.size(); }

 private:
  mutable std::map<std::string, std::string> secrets_;
};

struct TemporaryDatabaseDirectory {
  std::filesystem::path path = std::filesystem::temp_directory_path() /
      ("yukinal-desktop-host-" + std::to_string(
          std::chrono::steady_clock::now().time_since_epoch().count()));

  ~TemporaryDatabaseDirectory() {
    std::error_code error;
    std::filesystem::remove_all(path, error);
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
  // "Legacy" here is exactly the first eight shipping migrations. Building the
  // seed from the real SQL keeps it honest when a migration changes.
  for (std::size_t version = 0; version < 8; ++version) {
    const auto sql = std::string(yukinal::desktop_host::detail::kSchemaMigrations[version]);
    execute(database, sql.c_str());
  }
  execute(database, R"SQL(
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

    // Network proxy: the setting is non-secret, the credential is write-only, and
    // every rejection must leave both the database and the credential store alone.
    MemoryCredentialStore memory_credentials;
    yukinal::desktop_host::IpcRouter proxy_router(&database, &memory_credentials);
    const auto default_proxy = proxy_router.invoke("network_proxy_get", nlohmann::json::object());
    check(default_proxy.ok &&
              default_proxy.value == nlohmann::json{{"mode", "direct"},
                                                   {"hasCredential", false},
                                                   {"resolution", {{"kind", "direct"}}}},
          "network_proxy_get should default to an explicit direct route");
    check(!proxy_router.invoke("network_proxy_get", {{"unexpected", true}}).ok,
          "network_proxy_get should reject unexpected parameters");

    check(!proxy_router.invoke("network_proxy_save",
                               {{"input", {{"mode", "system"},
                                            {"credential", "user:pass"},
                                            {"clearCredential", true}}}}).ok,
          "a replacement credential and an explicit clear are mutually exclusive");
    check(!proxy_router.invoke("network_proxy_save",
                               {{"input", {{"mode", "direct"},
                                            {"credential", "user:pass"}}}}).ok,
          "a new proxy credential requires system mode");
    check(!proxy_router.invoke("network_proxy_save",
                               {{"input", {{"mode", "system"},
                                            {"credential", "no-colon"}}}}).ok,
          "a proxy credential without `user:password` must be refused");
    check(!proxy_router.invoke("network_proxy_save", {{"input", {{"mode", "sometimes"}}}}).ok,
          "an unknown proxy mode must be refused");
    check(!proxy_router.invoke("network_proxy_save",
                               {{"input", {{"mode", "system"}, {"extra", 1}}}}).ok,
          "unknown proxy input fields must be refused");
    check(memory_credentials.size() == 0 &&
              !database.network_proxy_config().credential_ref.has_value(),
          "a refused save must not store a credential");

    const auto saved_credential = proxy_router.invoke(
        "network_proxy_save",
        {{"input", {{"mode", "system"}, {"credential", "corp\\user:p@ss"}}}});
    check(saved_credential.ok, "saving a valid proxy credential should succeed");
    check(saved_credential.value.at("mode") == "system" &&
              saved_credential.value.at("hasCredential") == true,
          "the settings view should report the stored credential without its value");
    check(saved_credential.value.dump().find("p@ss") == std::string::npos,
          "the settings response must not carry the proxy credential");
    const auto stored_proxy = database.network_proxy_config();
    check(stored_proxy.mode == "system" && stored_proxy.credential_ref.has_value(),
          "the database should keep the mode and a credential reference");
    check(stored_proxy.credential_ref->starts_with("keychain://mcp/network-proxy-"),
          "SQLite must keep a credential-store reference, never the value");
    check(memory_credentials.size() == 1, "the credential should live in the credential store");

    const auto preserved = proxy_router.invoke(
        "network_proxy_save", {{"input", {{"mode", "system"}, {"credential", ""}}}});
    check(preserved.ok && preserved.value.at("hasCredential") == true &&
              database.network_proxy_config().credential_ref == stored_proxy.credential_ref,
          "an empty credential must preserve the stored one");

    const auto cleared = proxy_router.invoke(
        "network_proxy_save", {{"input", {{"mode", "direct"}, {"clearCredential", true}}}});
    check(cleared.ok && cleared.value.at("hasCredential") == false,
          "an explicit clear should remove the proxy credential");
    check(!database.network_proxy_config().credential_ref.has_value() &&
              memory_credentials.size() == 0,
          "a cleared credential must be reclaimed from the credential store");
  }

  sqlite3* readback = nullptr;
  const auto file = (temporary.path / "yukinal.db").string();
  check(sqlite3_open_v2(file.c_str(), &readback, SQLITE_OPEN_READONLY, nullptr) == SQLITE_OK,
        "could not reopen the temporary database read-only");
  check(query_integer(readback, "PRAGMA user_version") == 36,
        "opening a legacy database must migrate it to the supported schema version");
  check(query_integer(readback, "SELECT count(*) FROM pragma_table_info('servers')") == 22,
        "the legacy servers table should gain exactly the migrated host-trust columns");
  sqlite3_close(readback);

  TemporaryDatabaseDirectory absent;
  {
    DesktopDatabase database(absent.path);
    check(database.list_servers() == nlohmann::json{{"servers", nlohmann::json::array()}},
          "a new database should return an empty server list");
    check(database.list_workspaces() == nlohmann::json{{"workspaces", nlohmann::json::array()}},
          "a new database should return an empty workspace list");
    check(database.network_proxy_config().mode == "direct",
          "a new database should default the network proxy to direct");
  }
  sqlite3* fresh = nullptr;
  check(sqlite3_open_v2((absent.path / "yukinal.db").string().c_str(), &fresh,
                        SQLITE_OPEN_READONLY, nullptr) == SQLITE_OK,
        "a new database should be initialized on disk");
  check(query_integer(fresh, "PRAGMA user_version") == 36,
        "a new database should be migrated to the supported schema version");
  sqlite3_close(fresh);
  return 0;
}

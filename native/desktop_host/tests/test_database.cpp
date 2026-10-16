#include "yukinal/desktop_host/database.hpp"
#include "yukinal/desktop_host/desktop_mcp.hpp"
#include "yukinal/desktop_host/desktop_target.hpp"
#include "yukinal/desktop_host/host_trust.hpp"
#include "yukinal/desktop_host/ipc_router.hpp"
#include "schema_migrations.hpp"

#include <httplib.h>

#include <atomic>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <cstdlib>
#include <filesystem>
#include <iostream>
#include <map>
#include <memory>
#include <string>
#include <string_view>
#include <thread>

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

// Inserts one `file_transfers` row whose snapshot mirrors the row columns, the
// way the retired Rust transfer manager wrote them.
void insert_transfer(sqlite3* database, const std::string& id, const std::string& server_id,
                     const std::string& direction, const std::string& status,
                     std::int64_t started, std::int64_t updated) {
  const bool finished = status == "completed";
  const nlohmann::json snapshot = {
      {"transferId", id},       {"serverId", server_id},
      {"direction", direction}, {"status", status},
      {"startedAtEpochMs", started}, {"updatedAtEpochMs", updated},
      {"totalFiles", 1},         {"completedFiles", finished ? 1 : 0},
      {"skippedFiles", 0},       {"totalBytes", 10},
      {"transferredBytes", finished ? 10 : 4},
      {"currentItem", nullptr},  {"currentItemBytes", 0},
      {"currentItemTotalBytes", nullptr}, {"activeConflict", nullptr},
      {"verifiedFiles", finished ? 1 : 0}, {"unverifiedFiles", 0},
      {"failures", nlohmann::json::array()}, {"stagingResidue", nlohmann::json::array()},
  };
  const auto snapshot_text = snapshot.dump();
  sqlite3_stmt* statement = nullptr;
  check(sqlite3_prepare_v2(database,
                           "INSERT INTO file_transfers (transfer_id, server_id, direction, "
                           "status, started_at_epoch_ms, updated_at_epoch_ms, snapshot_json, "
                           "staging_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '[]')",
                           -1, &statement, nullptr) == SQLITE_OK,
        "could not prepare the file transfer fixture insert");
  sqlite3_bind_text(statement, 1, id.c_str(), -1, SQLITE_TRANSIENT);
  sqlite3_bind_text(statement, 2, server_id.c_str(), -1, SQLITE_TRANSIENT);
  sqlite3_bind_text(statement, 3, direction.c_str(), -1, SQLITE_TRANSIENT);
  sqlite3_bind_text(statement, 4, status.c_str(), -1, SQLITE_TRANSIENT);
  sqlite3_bind_int64(statement, 5, started);
  sqlite3_bind_int64(statement, 6, updated);
  sqlite3_bind_text(statement, 7, snapshot_text.c_str(), -1, SQLITE_TRANSIENT);
  check(sqlite3_step(statement) == SQLITE_DONE, "could not insert the file transfer fixture");
  sqlite3_finalize(statement);
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

// A minimal stdio MCP server used as the supervisor test fixture. The same test
// executable re-enters here when the supervisor spawns it with `--mcp-mock`.
int run_mcp_mock() {
  std::string line;
  while (std::getline(std::cin, line)) {
    auto frame = nlohmann::json::parse(line, nullptr, false);
    if (frame.is_discarded() || !frame.is_object() || !frame.contains("id")) continue;
    const auto id = frame.at("id");
    const auto method = frame.value("method", std::string());
    nlohmann::json response = {{"jsonrpc", "2.0"}, {"id", id}};
    if (method == "initialize") {
      response["result"] = {{"protocolVersion", "2024-11-05"},
                            {"capabilities", nlohmann::json::object()},
                            {"serverInfo", {{"name", "mock"}, {"version", "1.0"}}}};
    } else if (method == "tools/list") {
      const auto cursor = frame.value("params", nlohmann::json::object()).value("cursor", std::string());
      if (cursor.empty()) {
        response["result"] = {
            {"tools",
             nlohmann::json::array({nlohmann::json{{"name", "echo"},
                                                    {"description", "Echo a value"},
                                                    {"inputSchema", {{"type", "object"}}},
                                                    {"annotations", {{"readOnlyHint", true}}}}})},
            {"nextCursor", "page-2"}};
      } else {
        response["result"] = {
            {"tools",
             nlohmann::json::array({nlohmann::json{{"name", "secret"},
                                                    {"description", "Not reviewed"},
                                                    {"inputSchema", {{"type", "object"}}}}})}};
      }
    } else if (method == "tools/call") {
      const auto name = frame.at("params").value("name", std::string());
      response["result"] = {
          {"content", nlohmann::json::array({nlohmann::json{{"type", "text"},
                                                             {"text", "echo:" + name}}})},
          {"isError", false}};
    } else {
      response["error"] = {{"code", -32601}, {"message", "not found"}};
    }
    std::cout << response.dump() << "\n" << std::flush;
  }
  return 0;
}

// A stdio MCP server whose one reviewed tool blocks for ten seconds before it
// answers; the host cancellation test needs a call that is still in flight.
int run_mcp_slow_mock() {
  std::string line;
  while (std::getline(std::cin, line)) {
    auto frame = nlohmann::json::parse(line, nullptr, false);
    if (frame.is_discarded() || !frame.is_object() || !frame.contains("id")) continue;
    const auto id = frame.at("id");
    const auto method = frame.value("method", std::string());
    nlohmann::json response = {{"jsonrpc", "2.0"}, {"id", id}};
    if (method == "initialize") {
      response["result"] = {{"protocolVersion", "2024-11-05"},
                            {"capabilities", nlohmann::json::object()},
                            {"serverInfo", {{"name", "slow"}, {"version", "1.0"}}}};
    } else if (method == "tools/list") {
      response["result"] = {
          {"tools",
           nlohmann::json::array({nlohmann::json{{"name", "slow"},
                                                  {"description", "Blocks"},
                                                  {"inputSchema", {{"type", "object"}}}}})}};
    } else if (method == "tools/call") {
      std::this_thread::sleep_for(std::chrono::seconds(10));
      response["result"] = {
          {"content", nlohmann::json::array({nlohmann::json{{"type", "text"},
                                                             {"text", "slow-done"}}})},
          {"isError", false}};
    } else {
      response["error"] = {{"code", -32601}, {"message", "not found"}};
    }
    std::cout << response.dump() << "\n" << std::flush;
  }
  return 0;
}

}  // namespace

int main(int argc, char** argv) {
  using yukinal::desktop_host::DesktopDatabase;
  if (argc >= 2 && std::string(argv[1]) == "--mcp-mock") return run_mcp_mock();
  if (argc >= 2 && std::string(argv[1]) == "--mcp-slow-mock") return run_mcp_slow_mock();
  const std::filesystem::path test_executable =
      argc > 0 ? std::filesystem::path(argv[0]) : std::filesystem::path();

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

    // File transfers: the C++ host owns no transfer manager, so a persisted
    // in-progress row can only be stale and must read back as `interrupted`.
    {
      sqlite3* writer = nullptr;
      check(sqlite3_open((temporary.path / "yukinal.db").string().c_str(), &writer) == SQLITE_OK,
            "could not open the database for the file transfer fixture");
      insert_transfer(writer, "trf_completed", "srv_01abc", "upload", "completed", 1000, 1000);
      insert_transfer(writer, "trf_running", "srv_01abc", "download", "running", 500, 2000);
      insert_transfer(writer, "trf_other", "srv_other", "upload", "failed", 400, 400);
      sqlite3_close(writer);
    }
    const auto transfers = database.list_file_transfers(std::nullopt, 100);
    check(transfers.at("transfers").size() == 3,
          "list_file_transfers should return every stored row");
    check(transfers.at("transfers")[0].at("transferId") == "trf_running" &&
              transfers.at("transfers")[0].at("status") == "interrupted",
          "a persisted running transfer should reconcile to interrupted");
    check(transfers.at("transfers")[1].at("transferId") == "trf_completed" &&
              transfers.at("transfers")[1].at("status") == "completed",
          "a completed transfer should stay completed");
    check(database.list_file_transfers(std::string("srv_01abc"), 100).at("transfers").size() == 2,
          "list_file_transfers should filter by server id");
    check(database.list_file_transfers(std::nullopt, 1).at("transfers").size() == 1,
          "list_file_transfers should honor its bounded limit");
    const auto fetched = database.get_file_transfer("trf_running");
    check(fetched.has_value() && fetched->at("status") == "interrupted",
          "get_file_transfer should return the reconciled snapshot");
    check(!database.get_file_transfer("trf_missing").has_value(),
          "get_file_transfer should report an unknown transfer");
    check(database.list_file_transfers(std::nullopt, 1).at("transfers")[0].at("status") ==
              "interrupted",
          "the reconciliation must be persisted, not recomputed from a stale row");

    const auto transfer_list = router.invoke("file_transfer_list", {{"serverId", "srv_01abc"}});
    check(transfer_list.ok && transfer_list.value.at("transfers").size() == 2,
          "the C++ IPC router should expose the file transfer ledger");
    const auto transfer_get = router.invoke("file_transfer_get", {{"transferId", "trf_running"}});
    check(transfer_get.ok && transfer_get.value.at("transfer").at("status") == "interrupted",
          "the C++ IPC router should return the reconciled transfer");
    check(!router.invoke("file_transfer_get", {{"transferId", "trf_missing"}}).ok,
          "the C++ IPC router should report an unknown transfer");

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

    // Host-key trust: status and forget are local-only and must round-trip through
    // the same `known_hosts` file the connection path reads.
    const auto unpinned_status =
        router.invoke("server_host_key_status", {{"serverId", "srv_01abc"}});
    check(unpinned_status.ok &&
              unpinned_status.value == nlohmann::json{{"host", "ssh.example.test"},
                                                       {"port", 2222},
                                                       {"pinned", false}},
          "host-key status should resolve the server endpoint without a pin");
    check(!router.invoke("server_host_key_status", {{"serverId", "srv_missing"}}).ok,
          "host-key status should report an unknown server");
    check(!router.invoke("server_host_key_status",
                         {{"serverId", "srv_01abc"}, {"extra", true}}).ok,
          "host-key status should reject unexpected parameters");
    const auto forget_unpinned =
        router.invoke("server_host_key_forget", {{"serverId", "srv_01abc"}});
    check(forget_unpinned.ok && forget_unpinned.value.at("forgotten") == false,
          "forgetting an unpinned host is not an error and does not claim a removal");

    {
      auto store = yukinal::desktop_host::HostTrustStore::load(temporary.path / "known_hosts");
      store.pin("ssh.example.test", 2222, "SHA256:abc");
      check(store.check("ssh.example.test", 2222, "SHA256:other").comparison ==
                yukinal::desktop_host::HostKeyComparison::Mismatch,
            "a different presented key must be a mismatch, not a silent replacement");
    }
    const auto pinned_status =
        router.invoke("server_host_key_status", {{"serverId", "srv_01abc"}});
    check(pinned_status.ok && pinned_status.value.at("pinned") == true &&
              pinned_status.value.at("pinnedFingerprint") == "SHA256:abc",
          "host-key status should report the persisted pin");
    const auto forgot = router.invoke("server_host_key_forget", {{"serverId", "srv_01abc"}});
    check(forgot.ok && forgot.value.at("forgotten") == true,
          "forgetting a pinned host should report the removal");
    check(router.invoke("server_host_key_status", {{"serverId", "srv_01abc"}})
                  .value.at("pinned") == false,
          "the pin must be gone after forget");

    // Desktop SSH target resolver: the Python Agent gets a one-target payload
    // with secrets only in the credentials map. The C++ host never writes them
    // into the target JSON.
    MemoryCredentialStore target_credentials;
    const auto stored_password = target_credentials.set(
        "mcp", "target-root-password",
        yukinal::desktop_host::Secret{std::string_view("s3cret-pass")});
    {
      sqlite3* writer = nullptr;
      check(sqlite3_open((temporary.path / "yukinal.db").string().c_str(), &writer) == SQLITE_OK,
            "could not open the database for the SSH target fixture");
      execute(writer, R"SQL(
        INSERT INTO servers (
          id, name, host, port, username, identity_id, group_id, capabilities, status,
          environment, created_at, updated_at
        ) VALUES (
          'srv_target', 'Target', '43.240.221.2', 22, 'root', 'idn_target', NULL, '{}',
          'disconnected', 'staging', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'
        );
      )SQL");
      execute(writer, (std::string("INSERT INTO identities (id, label, method, credential_ref, created_at) VALUES ('idn_target', 'Root password', 'password', '") +
                       stored_password.to_string() +
                       "', '2026-01-01T00:00:00Z');").c_str());
      sqlite3_close(writer);
    }
    auto target_trust =
        yukinal::desktop_host::HostTrustStore::load(temporary.path / "known_hosts");
    target_trust.pin("43.240.221.2", 22, "SHA256:" + std::string(43, 'A'));

    const auto target_payload =
        yukinal::desktop_host::resolve_ssh_service_target(database, target_credentials,
                                                          "srv_target");
    check(target_payload.at("target").at("target_id") == "srv_target" &&
              target_payload.at("target").at("host") == "43.240.221.2" &&
              target_payload.at("target").at("port") == 22 &&
              target_payload.at("target").at("username") == "root",
          "the resolver should build the target from the stored connection");
    check(target_payload.at("target").at("auth").at("type") == "password" &&
              target_payload.at("target").at("auth").at("password_env") == "YK_SSH_PASSWORD",
          "a password identity should use the password_env auth shape");
    check(target_payload.at("credentials").at("YK_SSH_PASSWORD") == "s3cret-pass",
          "the password should travel only in the credentials map");
    check(target_payload.at("target").at("host_key_fingerprint") ==
              "SHA256:" + std::string(43, 'A'),
          "the confirmed pin should be the trust source");
    check(target_payload.at("target").dump().find("s3cret-pass") == std::string::npos,
          "the target JSON must never carry the secret value");

    bool unknown_target_rejected = false;
    try {
      (void)yukinal::desktop_host::resolve_ssh_service_target(database, target_credentials,
                                                              "srv_missing");
    } catch (const std::exception&) {
      unknown_target_rejected = true;
    }
    check(unknown_target_rejected, "an unknown server must be refused");

    const auto probe_payload = yukinal::desktop_host::resolve_ssh_service_probe_target(
        database, target_credentials, "srv_target");
    check(probe_payload.at("target").at("probe_only") == true &&
              probe_payload.at("target").at("auth").at("type") == "probe" &&
              !probe_payload.at("target").contains("host_key_fingerprint"),
          "the probe target must carry no identity and no trust source");

    const auto read_target = yukinal::desktop_host::resolve_ssh_service_filesystem_read_target(
        database, target_credentials,
        {{"serverId", "srv_target"}, {"path", "/etc/hosts"}});
    check(read_target.at("filesystemRead").at("path") == "/etc/hosts" &&
              read_target.at("filesystemRead").at("maxBytes") == 131072,
          "a filesystem read target should default to the bounded read size");
    const auto bounded_read = yukinal::desktop_host::resolve_ssh_service_filesystem_read_target(
        database, target_credentials,
        {{"serverId", "srv_target"}, {"path", "/etc/hosts"}, {"maxBytes", 1024}});
    check(bounded_read.at("filesystemRead").at("maxBytes") == 1024,
          "a filesystem read target should honor an explicit maxBytes");

    auto expect_rejected = [&database, &target_credentials](const char* label,
                                                            const nlohmann::json& request) {
      bool rejected = false;
      try {
        (void)yukinal::desktop_host::resolve_ssh_service_filesystem_read_target(
            database, target_credentials, request);
      } catch (const std::exception&) {
        rejected = true;
      }
      check(rejected, label);
    };
    expect_rejected("a relative read path must be refused",
                    {{"serverId", "srv_target"}, {"path", "etc/hosts"}});
    expect_rejected("a credential path must be refused by the blocklist",
                    {{"serverId", "srv_target"}, {"path", "/home/deploy/.ssh/id_rsa"}});
    expect_rejected("an over-limit maxBytes must be refused",
                    {{"serverId", "srv_target"},
                     {"path", "/etc/hosts"},
                     {"maxBytes", 2 * 1024 * 1024}});

    const auto write_target = yukinal::desktop_host::resolve_ssh_service_filesystem_write_target(
        database, target_credentials,
        {{"serverId", "srv_target"}, {"path", "/srv/app.conf"}, {"content", "PORT=8080\n"}});
    check(write_target.at("filesystemWrite").at("bytesWritten") == 10,
          "a filesystem write target should report the validated byte count");

    const auto revision = std::string(64, 'a');
    const auto edit_target = yukinal::desktop_host::resolve_ssh_service_filesystem_edit_target(
        database, target_credentials,
        {{"serverId", "srv_target"},
         {"path", "/srv/app.conf"},
         {"expectedRevision", revision},
         {"oldString", "PORT=8080"},
         {"newString", "PORT=9090"}});
    check(edit_target.at("filesystemEdit").at("expectedRevision") == revision,
          "a filesystem edit target should carry the validated revision");
    bool bad_revision_rejected = false;
    try {
      (void)yukinal::desktop_host::resolve_ssh_service_filesystem_edit_target(
          database, target_credentials,
          {{"serverId", "srv_target"},
           {"path", "/srv/app.conf"},
           {"expectedRevision", "short"},
           {"oldString", "a"},
           {"newString", "b"}});
    } catch (const std::exception&) {
      bad_revision_rejected = true;
    }
    check(bad_revision_rejected, "a malformed edit revision must be refused");

    // MCP server settings: list/save/delete with write-only static header secrets.
    MemoryCredentialStore mcp_credentials;
    yukinal::desktop_host::IpcRouter mcp_router(&database, &mcp_credentials);
    const auto empty_mcp = mcp_router.invoke("mcp_server_list", nlohmann::json::object());
    check(empty_mcp.ok && empty_mcp.value.at("servers").empty(),
          "mcp_server_list should start empty");

    nlohmann::json header = {{"name", "Authorization"}, {"secret", "Bearer super-secret"}};
    nlohmann::json stdio_input = {{"id", "ops-tools"},
                                  {"label", "Ops tools"},
                                  {"transport", "stdio"},
                                  {"command", "npx"},
                                  {"args", nlohmann::json::array({"-y", "ops-mcp"})},
                                  {"enabled", true},
                                  {"httpAuthHeaders", nlohmann::json::array({header})}};
    nlohmann::json save_params = {{"input", stdio_input}};
    const auto saved_mcp = mcp_router.invoke("mcp_server_save", save_params);
    check(saved_mcp.ok, "mcp_server_save should accept a stdio server");
    check(saved_mcp.value.at("config").at("id") == "ops-tools" &&
              saved_mcp.value.at("status").at("running") == false &&
              !saved_mcp.value.contains("unavailable"),
          "a saved enabled MCP server should be returned available and not running");
    check(saved_mcp.value.dump().find("super-secret") == std::string::npos,
          "the header secret must never be returned");
    check(mcp_credentials.size() == 1,
          "the header secret should live in the credential store");
    const auto& saved_header = saved_mcp.value.at("config").at("httpAuthHeaders").at(0);
    check(saved_header.at("credentialRef").is_string() &&
              !saved_header.at("credentialRef").get<std::string>().empty(),
          "the header should carry a credential reference, not the value");

    const auto listed_mcp = mcp_router.invoke("mcp_server_list", nlohmann::json::object());
    check(listed_mcp.ok && listed_mcp.value.at("servers").size() == 1,
          "mcp_server_list should return the saved server");

    nlohmann::json disabled_input = {{"id", "ops-tools"},
                                     {"label", "Ops tools"},
                                     {"transport", "stdio"},
                                     {"command", "npx"},
                                     {"enabled", false}};
    const auto disabled_mcp =
        mcp_router.invoke("mcp_server_save", {{"input", disabled_input}});
    check(disabled_mcp.ok && disabled_mcp.value.at("unavailable").at("code") == "disabled",
          "a disabled MCP server should report the disabled reason");
    check(mcp_credentials.size() == 1,
          "a save without a new secret must preserve the stored header credential");

    nlohmann::json bad_input = {{"id", "bad"},
                                {"label", "Bad"},
                                {"transport", "carrier-pigeon"},
                                {"enabled", true}};
    check(!mcp_router.invoke("mcp_server_save", {{"input", bad_input}}).ok,
          "an unknown MCP transport must be refused");

    nlohmann::json oauth_input = {{"id", "oauth"},
                                  {"label", "OAuth"},
                                  {"transport", "http"},
                                  {"url", "https://example.test/mcp"},
                                  {"enabled", true},
                                  {"oauth",
                                   {{"issuer", "https://example.test"},
                                    {"clientId", ""},
                                    {"scopes", nlohmann::json::array()}}}};
    check(!mcp_router.invoke("mcp_server_save", {{"input", oauth_input}}).ok,
          "OAuth MCP servers are not supported by this host yet");

    const auto reviewed_mcp = mcp_router.invoke(
        "mcp_server_review",
        {{"serverId", "ops-tools"},
         {"allowedTools", nlohmann::json::array({"echo"})},
         {"trustLevel", "reviewed"},
         {"annotationTrust", "trusted"}});
    check(reviewed_mcp.ok && reviewed_mcp.value.at("config").at("allowedTools").size() == 1 &&
              reviewed_mcp.value.at("config").at("trustLevel") == "reviewed" &&
              reviewed_mcp.value.at("config").at("annotationTrust") == "trusted",
          "mcp_server_review should record the reviewed tool surface");
    check(!mcp_router
               .invoke("mcp_server_review",
                       {{"serverId", "ops-tools"},
                        {"allowedTools", nlohmann::json::array({"echo", "echo"})},
                        {"trustLevel", "reviewed"}})
               .ok,
          "mcp_server_review should reject duplicate tool names");

    const auto deleted_mcp =
        mcp_router.invoke("mcp_server_delete", {{"serverId", "ops-tools"}});
    check(deleted_mcp.ok && deleted_mcp.value.at("deleted") == true &&
              deleted_mcp.value.at("stopped") == false,
          "mcp_server_delete should remove the row and report nothing running");
    check(mcp_credentials.size() == 0,
          "deleting an MCP server should reclaim its header secret");
    check(!mcp_router.invoke("mcp_server_delete", {{"serverId", "ops-tools"}}).ok,
          "deleting an unknown MCP server should fail");

    // Snapshot persistence: the overview pane stores its normalized contract.
    database.insert_snapshot({{"id", "snap_srv_01abc_1"},
                              {"serverId", "srv_01abc"},
                              {"collectedAt", "2026-01-01T00:00:00Z"},
                              {"health", "healthy"},
                              {"capabilities", {{"linux", true}, {"docker", false}}},
                              {"collectors", nlohmann::json::array()}});
    {
      sqlite3* snapshot_db = nullptr;
      check(sqlite3_open((temporary.path / "yukinal.db").string().c_str(), &snapshot_db) ==
                SQLITE_OK,
            "could not reopen the database for the snapshot readback");
      check(query_integer(
                snapshot_db,
                "SELECT count(*) FROM snapshots WHERE id = 'snap_srv_01abc_1'") == 1,
            "insert_snapshot should persist exactly one row");
      sqlite3_close(snapshot_db);
    }
    bool bad_snapshot_rejected = false;
    try {
      database.insert_snapshot({{"id", "snap_bad"},
                                {"serverId", "srv_01abc"},
                                {"collectedAt", "2026-01-01T00:00:00Z"},
                                {"health", "not-a-health"}});
    } catch (const std::exception&) {
      bad_snapshot_rejected = true;
    }
    check(bad_snapshot_rejected, "insert_snapshot should reject an unknown health value");

    // Connection lifecycle: disconnect is idempotent, and the keyboard-interactive
    // commands answer an idle desktop with accepted:false.
    const auto disconnected = router.invoke("server_disconnect", {{"serverId", "srv_01abc"}});
    check(disconnected.ok, "server_disconnect should succeed");
    const auto idle_respond = router.invoke(
        "server_auth_respond",
        {{"authId", "auth_probe"}, {"responses", nlohmann::json::array({"otp"})}});
    check(idle_respond.ok && idle_respond.value.at("accepted") == false,
          "server_auth_respond should answer an idle desktop with accepted:false");
    const auto idle_cancel = router.invoke("server_auth_cancel", {{"authId", "auth_probe"}});
    check(idle_cancel.ok && idle_cancel.value.at("accepted") == false,
          "server_auth_cancel should answer an idle desktop with accepted:false");
    check(!router.invoke("server_auth_respond", {{"authId", "auth_probe"}}).ok,
          "server_auth_respond should require responses");
    check(!router.invoke("server_auth_cancel", {{"authId", ""}}).ok,
          "server_auth_cancel should reject an empty challenge id");
    check(!router.invoke("server_disconnect", {{"serverId", "srv_missing"}}).ok,
          "server_disconnect should reject an unknown server");

    // MCP supervisor: spawn this test executable as a stdio MCP server and verify
    // the catalog and one tool call end to end.
    if (!test_executable.empty()) {
      database.upsert_mcp_server({{"id", "mcp_mock"},
                                  {"label", "Mock"},
                                  {"transport", "stdio"},
                                  {"command", test_executable.string()},
                                  {"args", nlohmann::json::array({"--mcp-mock"})},
                                  {"enabled", true},
                                  {"allowedTools", nlohmann::json::array({"echo"})},
                                  {"trustLevel", "reviewed"},
                                  {"annotationTrust", "trusted"}});
      yukinal::desktop_host::McpSupervisor supervisor(&database, &mcp_credentials);
      const auto catalog = supervisor.catalog();
      check(catalog.at("servers").size() == 1 &&
                catalog.at("servers").at(0).at("tools").size() == 1,
            "the MCP catalog should expose the one reviewed tool");
      const auto& tool = catalog.at("servers").at(0).at("tools").at(0);
      check(tool.at("name") == "mcp.mcp_mock.echo" && tool.at("risk") == "low",
            "a reviewed read-only MCP tool should be low risk");
      const auto output = supervisor.call("mcp.mcp_mock.echo", nlohmann::json::object());
      check(output.at("serverId") == "mcp_mock" && output.at("isError") == false &&
                output.at("text") == "echo:echo",
            "the MCP tool call should return its content");
      bool bad_name_rejected = false;
      try {
        (void)supervisor.call("mcp.mcp_mock.missing", nlohmann::json::object());
      } catch (const std::exception&) {
        bad_name_rejected = true;
      }
      check(bad_name_rejected, "an unknown MCP tool should be refused");
      bool unreviewed_rejected = false;
      try {
        (void)supervisor.call("mcp.mcp_mock.secret", nlohmann::json::object());
      } catch (const std::exception&) {
        unreviewed_rejected = true;
      }
      check(unreviewed_rejected,
            "a tool that was not reviewed must not be callable even if the server lists it");
      supervisor.stop("mcp_mock");
      // The stdio server row stays in the database; disable it so the HTTP
      // catalog below only starts the server it is testing.
      auto stdio_row = database.get_mcp_server("mcp_mock").value();
      stdio_row["enabled"] = false;
      database.upsert_mcp_server(stdio_row);
    }

    // MCP call cancellation: a reviewed tool that blocks for ten seconds is
    // abandoned as soon as its cancellation token is set, and the call returns
    // promptly instead of waiting out the ten seconds.
    if (!test_executable.empty()) {
      database.upsert_mcp_server({{"id", "mcp_slow"},
                                  {"label", "Slow"},
                                  {"transport", "stdio"},
                                  {"command", test_executable.string()},
                                  {"args", nlohmann::json::array({"--mcp-slow-mock"})},
                                  {"enabled", true},
                                  {"allowedTools", nlohmann::json::array({"slow"})},
                                  {"trustLevel", "reviewed"},
                                  {"annotationTrust", "trusted"}});
      yukinal::desktop_host::McpSupervisor slow_supervisor(&database, &mcp_credentials);
      const auto slow_catalog = slow_supervisor.catalog();
      check(slow_catalog.at("servers").size() == 1 &&
                slow_catalog.at("servers").at(0).at("serverId") == "mcp_slow",
            "the slow MCP server should be the only one in its catalog");
      auto cancel = std::make_shared<std::atomic<bool>>(false);
      std::thread canceler([&cancel] {
        std::this_thread::sleep_for(std::chrono::milliseconds(300));
        cancel->store(true);
      });
      bool cancelled = false;
      const auto started = std::chrono::steady_clock::now();
      try {
        (void)slow_supervisor.call("mcp.mcp_slow.slow", nlohmann::json::object(), cancel);
      } catch (const std::exception&) {
        cancelled = true;
      }
      const auto elapsed = std::chrono::steady_clock::now() - started;
      canceler.join();
      check(cancelled, "a cancelled MCP tool call should abort the waiting call");
      check(elapsed < std::chrono::seconds(5),
            "a cancelled MCP tool call should return without waiting for the server");
      // Disable the slow row so the HTTP catalog below stays hermetic.
      auto slow_row = database.get_mcp_server("mcp_slow").value();
      slow_row["enabled"] = false;
      database.upsert_mcp_server(slow_row);
      slow_supervisor.stop("mcp_slow");
    }

    // MCP Streamable HTTP transport: one loopback httplib server answers the
    // handshake over JSON, tools/list over an SSE stream, and tools/call over
    // JSON, and requires both the static Authorization header and the issued
    // session id on every follow-up request.
    {
      httplib::Server http_mcp;
      std::atomic<bool> saw_session{false};
      std::atomic<bool> saw_delete{false};
      http_mcp.Delete("/mcp", [&](const httplib::Request& request, httplib::Response& response) {
        if (request.get_header_value("Authorization") != "Bearer s3cret") {
          response.status = 401;
          return;
        }
        if (request.get_header_value("Mcp-Session-Id") == "sess-http-1") saw_delete = true;
        response.status = 200;
      });
      http_mcp.Post("/mcp", [&](const httplib::Request& request, httplib::Response& response) {
        if (request.get_header_value("Authorization") != "Bearer s3cret") {
          response.status = 401;
          return;
        }
        auto frame = nlohmann::json::parse(request.body, nullptr, false);
        if (frame.is_discarded() || !frame.is_object()) {
          response.status = 400;
          return;
        }
        const auto method = frame.value("method", std::string());
        response.set_header("Mcp-Session-Id", "sess-http-1");
        if (method == "initialize") {
          response.set_content(
              nlohmann::json{{"jsonrpc", "2.0"},
                             {"id", frame.at("id")},
                             {"result",
                              {{"protocolVersion", "2024-11-05"},
                               {"capabilities", nlohmann::json::object()},
                               {"serverInfo", {{"name", "http-mock"}, {"version", "1.0"}}}}}}
                  .dump(),
              "application/json");
          return;
        }
        if (method == "notifications/initialized") {
          response.status = 202;
          return;
        }
        if (!frame.contains("id")) {
          response.status = 400;
          return;
        }
        if (request.get_header_value("Mcp-Session-Id") == "sess-http-1") saw_session = true;
        if (method == "tools/list") {
          // Answer over SSE so the transport's event-stream decoding is covered.
          const auto reply =
              nlohmann::json{{"jsonrpc", "2.0"},
                             {"id", frame.at("id")},
                             {"result",
                              {{"tools", nlohmann::json::array({nlohmann::json{
                                             {"name", "echo"},
                                             {"description", "Echo a value"},
                                             {"inputSchema", {{"type", "object"}}},
                                             {"annotations", {{"readOnlyHint", true}}}}})}}}}
                  .dump();
          response.set_content("data: " + reply + "\n\n", "text/event-stream");
          return;
        }
        if (method == "tools/call") {
          const auto name = frame.at("params").value("name", std::string());
          response.set_content(
              nlohmann::json{{"jsonrpc", "2.0"},
                             {"id", frame.at("id")},
                             {"result",
                              {{"content", nlohmann::json::array({nlohmann::json{
                                                 {"type", "text"}, {"text", "http:" + name}}})},
                               {"isError", false}}}}
                  .dump(),
              "application/json");
          return;
        }
        response.status = 400;
      });
      const int http_port = http_mcp.bind_to_any_port("127.0.0.1");
      check(http_port > 0, "the MCP HTTP mock could not bind a loopback port");
      std::thread http_mock_thread([&http_mcp] { http_mcp.listen_after_bind(); });
      http_mcp.wait_until_ready();
      const auto header_reference =
          mcp_credentials
              .set("mcp", "mcp-header-test", yukinal::desktop_host::Secret{"Bearer s3cret"})
              .to_string();
      database.upsert_mcp_server(
          {{"id", "mcp_http_mock"},
           {"label", "HTTP Mock"},
           {"transport", "http"},
           {"url", "http://127.0.0.1:" + std::to_string(http_port) + "/mcp"},
           {"enabled", true},
           {"allowedTools", nlohmann::json::array({"echo"})},
           {"trustLevel", "reviewed"},
           {"annotationTrust", "trusted"},
           {"httpAuthHeaders",
            nlohmann::json::array({nlohmann::json{{"name", "Authorization"},
                                                  {"credentialRef", header_reference}}})}});
      yukinal::desktop_host::McpSupervisor http_supervisor(&database, &mcp_credentials);
      const auto http_catalog = http_supervisor.catalog();
      check(http_catalog.at("servers").size() == 1 &&
                http_catalog.at("servers").at(0).at("serverId") == "mcp_http_mock" &&
                http_catalog.at("servers").at(0).at("tools").size() == 1 &&
                http_catalog.at("servers").at(0).at("tools").at(0).at("name") ==
                    "mcp.mcp_http_mock.echo",
            "the HTTP MCP catalog should expose the reviewed tool over SSE");
      check(http_catalog.at("servers").at(0).at("tools").at(0).at("risk") == "low",
            "a reviewed read-only HTTP MCP tool should be low risk");
      check(saw_session.load(), "the HTTP MCP session must carry the issued Mcp-Session-Id");
      const auto http_output =
          http_supervisor.call("mcp.mcp_http_mock.echo", nlohmann::json::object());
      check(http_output.at("serverId") == "mcp_http_mock" &&
                http_output.at("isError") == false && http_output.at("text") == "http:echo",
            "the HTTP MCP tool call should return its content");
      http_supervisor.stop("mcp_http_mock");
      check(saw_delete.load(), "stopping an HTTP MCP server should terminate its session");
      http_mcp.stop();
      http_mock_thread.join();
    }

    // Investigation task list: host-owned rows decode into the shared contract.
    {
      sqlite3* writer = nullptr;
      check(sqlite3_open((temporary.path / "yukinal.db").string().c_str(), &writer) == SQLITE_OK,
            "could not open the database for the investigation fixture");
      execute(writer, R"SQL(
        INSERT INTO investigation_tasks (
          id, workspace_id, server_id, objective, success_criteria, scope, mode,
          permission_mode, automation_level, status, max_steps, max_run_ms, created_at,
          updated_at, max_attempts, created_by, phase, guardrails
        ) VALUES (
          'itask_1', NULL, 'srv_01abc', 'Check nginx', '["nginx is active"]',
          '{"host":"remote","serverId":"srv_01abc","environment":"staging"}',
          'goal', 'ask', 'propose', 'investigating', 8, 120000,
          '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 3, 'user', 'investigating',
          '{"forbiddenTools":[],"forbiddenPathPrefixes":[]}'
        );
      )SQL");
      execute(writer, R"SQL(
        INSERT INTO investigation_findings (
          id, task_id, title, kind, statement, evidence_ids, confidence, created_at
        ) VALUES (
          'ifind_1', 'itask_1', 'Nginx active', 'fact', 'nginx is active', '[]', 'high',
          '2026-01-01T00:01:00Z'
        );
        INSERT INTO investigation_plans (
          id, task_id, revision, status, created_at, updated_at, current_step_id, steps
        ) VALUES (
          'iplan_1', 'itask_1', 1, 'active', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z',
          NULL, '[]'
        );
      )SQL");
      sqlite3_close(writer);
    }
    const auto tasks = router.invoke("investigation_task_list", nlohmann::json::object());
    check(tasks.ok && tasks.value.at("tasks").size() == 1,
          "investigation_task_list should return the stored task");
    const auto& task = tasks.value.at("tasks").at(0);
    check(task.at("id") == "itask_1" && task.at("objective") == "Check nginx" &&
              task.at("status") == "investigating" && task.at("phase") == "investigating" &&
              task.at("budget").at("maxAttempts") == 3 &&
              task.at("successCriteria").size() == 1 && task.at("scope").at("host") == "remote",
          "investigation_task_list should decode the shared task contract");
    check(router.invoke("investigation_task_list", {{"status", "completed"}})
                  .value.at("tasks")
                  .empty(),
          "investigation_task_list should filter by status");
    check(!router.invoke("investigation_task_list", {{"status", "bogus"}}).ok,
          "investigation_task_list should reject an unknown status");

    const auto detail = router.invoke("investigation_task_get", {{"taskId", "itask_1"}});
    check(detail.ok && detail.value.at("task").at("id") == "itask_1" &&
              detail.value.at("findings").size() == 1 &&
              detail.value.at("findings").at(0).at("title") == "Nginx active" &&
              detail.value.at("plan").at("id") == "iplan_1" &&
              detail.value.at("evidence").empty() && detail.value.at("runs").empty() &&
              detail.value.at("steps").empty() && detail.value.at("artifacts").empty(),
          "investigation_task_get should return the task, findings and active plan");
    check(!router.invoke("investigation_task_get", {{"taskId", "itask_missing"}}).ok,
          "investigation_task_get should report an unknown task");

    const auto status_update = router.invoke(
        "investigation_task_status_update",
        {{"taskId", "itask_1"}, {"status", "waiting_user"}});
    check(status_update.ok && status_update.value.at("task").at("status") == "waiting_user" &&
              !status_update.value.at("task").contains("completedAt"),
          "a non-terminal status update should clear the completion time");
    const auto stopped_task =
        router.invoke("investigation_task_stop", {{"taskId", "itask_1"}});
    check(stopped_task.ok && stopped_task.value.at("task").at("status") == "stopped" &&
              stopped_task.value.at("task").contains("completedAt"),
          "investigation_task_stop should stop an active task and record completion");
    const auto stopped_again =
        router.invoke("investigation_task_stop", {{"taskId", "itask_1"}});
    check(stopped_again.ok && stopped_again.value.at("task").at("status") == "stopped",
          "stopping an already-stopped task should be idempotent");
    check(!router.invoke("investigation_task_status_update",
                         {{"taskId", "itask_1"}, {"status", "bogus"}})
               .ok,
          "investigation_task_status_update should reject an unknown status");
    check(!router.invoke("investigation_task_stop", {{"taskId", "itask_missing"}}).ok,
          "investigation_task_stop should report an unknown task");

    // ── investigation_task_create ────────────────────────────────────────────
    // The create command normalizes the bounded input, reconciles the scope ids
    // and installs host-issued command delegation only for eligible targets.
    {
      const nlohmann::json remote_scope = {
          {"host", "remote"}, {"serverId", "srv_01abc"}, {"environment", "staging"}};
      const nlohmann::json base_input = {
          {"objective", "  Verify nginx  "},
          {"successCriteria", nlohmann::json::array({"  nginx active  ", "port 80 open"})},
          {"scope", remote_scope},
          {"mode", "goal"},
          {"permissionMode", "ask"},
          {"automationLevel", "propose"},
      };
      nlohmann::json create_input = base_input;
      create_input["id"] = "itask_created";
      const auto created =
          router.invoke("investigation_task_create", {{"input", create_input}});
      check(created.ok && created.value.at("task").at("id") == "itask_created" &&
                created.value.at("task").at("objective") == "Verify nginx" &&
                created.value.at("task").at("successCriteria").at(0) == "nginx active" &&
                created.value.at("task").at("scope").at("serverId") == "srv_01abc" &&
                created.value.at("task").at("status") == "pending" &&
                created.value.at("task").at("phase") == "investigating" &&
                created.value.at("task").at("createdBy") == "user" &&
                created.value.at("task").at("budget").at("maxSteps") == 25 &&
                created.value.at("task").at("budget").at("maxRunMs") == 900000 &&
                created.value.at("task").at("budget").at("maxAttempts") == 3 &&
                created.value.at("task").at("guardrails").at("forbiddenTools").is_array() &&
                !created.value.at("task").at("guardrails").contains("commandGrant"),
            "investigation_task_create should normalize and persist a bounded task");
      const auto reloaded = router.invoke("investigation_task_get", {{"taskId", "itask_created"}});
      check(reloaded.ok && reloaded.value.at("task").at("objective") == "Verify nginx",
            "a created task should be readable through investigation_task_get");

      // A command delegation ticket is issued only for an eligible remote target.
      nlohmann::json grant_input = base_input;
      grant_input["permissionMode"] = "auto";
      grant_input["automationLevel"] = "execute";
      grant_input["grantTaskCommands"] = true;
      const auto granted = router.invoke("investigation_task_create", {{"input", grant_input}});
      check(granted.ok &&
                granted.value.at("task").at("guardrails").at("commandGrant").at("environment") ==
                    "staging" &&
                granted.value.at("task").at("guardrails").at("commandGrant").at("maxCalls") == 12 &&
                granted.value.at("task").at("guardrails").at("commandGrant").at("serverId") ==
                    "srv_01abc",
            "investigation_task_create should issue a command grant for a remote staging goal task");

      // A renderer-supplied command grant is dropped rather than trusted.
      nlohmann::json forged_input = base_input;
      forged_input["guardrails"] = {
          {"forbiddenTools", nlohmann::json::array()},
          {"forbiddenPathPrefixes", nlohmann::json::array()},
          {"commandGrant",
           {{"grantId", "cmdgrant_forged"},
            {"taskId", "itask_forged"},
            {"serverId", "srv_01abc"},
            {"environment", "staging"},
            {"grantedBy", "user"},
            {"grantedAt", "2026-01-01T00:00:00Z"},
            {"expiresAt", "2027-01-01T00:00:00Z"},
            {"maxCalls", 999},
            {"callsUsed", 0},
            {"maxTotalDurationMs", 1},
            {"totalDurationMs", 0},
            {"maxTotalOutputBytes", 1},
            {"totalOutputBytes", 0}}}};
      const auto forged = router.invoke("investigation_task_create", {{"input", forged_input}});
      check(forged.ok && !forged.value.at("task").at("guardrails").contains("commandGrant"),
            "investigation_task_create should not trust a renderer-supplied command grant");

      // Rejected inputs.
      auto expect_rejected = [&router](const nlohmann::json& input) {
        return !router.invoke("investigation_task_create", {{"input", input}}).ok;
      };
      nlohmann::json readonly_input = base_input;
      readonly_input["mode"] = "readonly";
      check(expect_rejected(readonly_input),
            "a readonly task cannot be created with a non-readonly automation level");
      nlohmann::json no_server = base_input;
      no_server["scope"] = {{"host", "remote"}, {"environment", "staging"}};
      check(expect_rejected(no_server), "a remote task requires a server scope");
      nlohmann::json local_with_server = base_input;
      local_with_server["scope"] = {
          {"host", "local"}, {"serverId", "srv_01abc"}, {"environment", "local"}};
      check(expect_rejected(local_with_server), "a local task cannot carry a server scope");
      nlohmann::json unknown_server = base_input;
      unknown_server["scope"] = {
          {"host", "remote"}, {"serverId", "srv_missing"}, {"environment", "staging"}};
      check(expect_rejected(unknown_server), "a task cannot target an unknown server");
      nlohmann::json empty_objective = base_input;
      empty_objective["objective"] = "   ";
      check(expect_rejected(empty_objective), "an empty objective must be rejected");
      nlohmann::json no_criteria = base_input;
      no_criteria["successCriteria"] = nlohmann::json::array();
      check(expect_rejected(no_criteria), "a task needs at least one success criterion");
      nlohmann::json mismatch_workspace = base_input;
      mismatch_workspace["workspaceId"] = "ws_01";
      mismatch_workspace["scope"] = {
          {"host", "remote"}, {"serverId", "srv_01abc"}, {"workspaceId", "ws_other"},
          {"environment", "staging"}};
      check(expect_rejected(mismatch_workspace),
            "an explicit workspace id must agree with the task scope");
      nlohmann::json bad_grant = base_input;
      bad_grant["grantTaskCommands"] = true;
      check(expect_rejected(bad_grant),
            "command delegation is unavailable for a non-executable task");
      nlohmann::json wrong_environment = base_input;
      wrong_environment["permissionMode"] = "auto";
      wrong_environment["automationLevel"] = "execute";
      wrong_environment["grantTaskCommands"] = true;
      wrong_environment["scope"] = {
          {"host", "remote"}, {"serverId", "srv_01abc"}, {"environment", "development"}};
      check(expect_rejected(wrong_environment),
            "command delegation must match the registered server environment");
      nlohmann::json unknown_field = base_input;
      unknown_field["surprise"] = true;
      check(expect_rejected(unknown_field), "an unknown create field must be rejected");
      check(!router.invoke("investigation_task_create", nlohmann::json::object()).ok,
            "investigation_task_create requires an input object");
      check(!router.invoke("investigation_task_create", {{"input", "nope"}}).ok,
            "investigation_task_create input must be an object");

      // A workspace-scoped task reconciles against a registered workspace.
      nlohmann::json workspace_input = base_input;
      workspace_input["scope"] = {{"host", "remote"},
                                  {"serverId", "srv_01abc"},
                                  {"workspaceId", "ws_01"},
                                  {"environment", "staging"}};
      const auto workspace_task =
          router.invoke("investigation_task_create", {{"input", workspace_input}});
      check(workspace_task.ok &&
                workspace_task.value.at("task").at("workspaceId") == "ws_01" &&
                workspace_task.value.at("task").at("scope").at("workspaceId") == "ws_01",
            "investigation_task_create should accept a registered workspace scope");
      nlohmann::json unknown_workspace = workspace_input;
      unknown_workspace["scope"] = {{"host", "remote"},
                                    {"serverId", "srv_01abc"},
                                    {"workspaceId", "ws_missing"},
                                    {"environment", "staging"}};
      check(expect_rejected(unknown_workspace),
            "a task cannot target an unknown workspace");
    }

    // ── Investigation write foundation ───────────────────────────────────────
    // Durable tasks, plans, evidence, findings and briefs round-trip through the
    // shared JSON contract and enforce the hash and redaction guards.
    {
      const nlohmann::json content = {{"path", "/etc/nginx/nginx.conf"}, {"active", true}};
      const auto content_hash = DesktopDatabase::content_hash(content);
      check(DesktopDatabase::content_hash(nlohmann::json{{"a", 1}}) ==
                "015abd7f5cc57a2dd94b7590f04ad8084273905ee33ec5cebeae62276a97f862",
            "content_hash should be the SHA-256 of the canonical JSON serialization");
      check(content_hash.size() == 64 &&
                content_hash != DesktopDatabase::content_hash(nlohmann::json{{"a", 2}}),
            "content_hash should be a stable lowercase hex digest");

      const nlohmann::json task = {
          {"id", "itask_write"},
          {"serverId", "srv_01abc"},
          {"objective", "Verify nginx"},
          {"successCriteria", nlohmann::json::array({"nginx active"})},
          {"scope",
           {{"host", "remote"}, {"serverId", "srv_01abc"}, {"environment", "staging"}}},
          {"guardrails",
           {{"forbiddenTools", nlohmann::json::array()},
            {"forbiddenPathPrefixes", nlohmann::json::array()}}},
          {"mode", "goal"},
          {"permissionMode", "ask"},
          {"automationLevel", "propose"},
          {"phase", "investigating"},
          {"status", "pending"},
          {"budget", {{"maxSteps", 8}, {"maxRunMs", 120000}, {"maxAttempts", 3}}},
          {"createdBy", "user"},
          {"createdAt", "2026-01-01T00:00:00Z"},
          {"updatedAt", "2026-01-01T00:00:00Z"},
      };
      const auto created = database.create_investigation_task(task);
      check(created.has_value() && created->at("id") == "itask_write" &&
                created->at("status") == "pending" && created->at("phase") == "investigating" &&
                created->at("budget").at("maxAttempts") == 3 &&
                created->at("scope").at("environment") == "staging" &&
                created->at("guardrails").contains("forbiddenTools"),
            "create_investigation_task should persist and echo the shared task contract");
      bool missing_id_rejected = false;
      try {
        (void)database.create_investigation_task(nlohmann::json{{"objective", "x"}});
      } catch (const std::invalid_argument&) {
        missing_id_rejected = true;
      }
      check(missing_id_rejected, "create_investigation_task should require an id");

      // Evidence and runs are linked by foreign key, so the write path needs a real run
      // row before evidence can carry a run id.
      {
        sqlite3* writer = nullptr;
        check(sqlite3_open((temporary.path / "yukinal.db").string().c_str(), &writer) == SQLITE_OK,
              "could not open the database for the investigation run fixture");
        execute(writer, R"SQL(
          INSERT INTO investigation_runs (
            id, task_id, attempt, phase, status, started_at, updated_at
          ) VALUES (
            'run_1', 'itask_write', 1, 'investigating', 'running',
            '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'
          );
        )SQL");
        sqlite3_close(writer);
      }

      const auto progressed = database.update_investigation_task_progress(
          "itask_write", "investigating", "recovery", std::string("run_1"), std::nullopt,
          "2026-01-01T00:00:30Z", std::nullopt);
      check(progressed.has_value() && progressed->at("status") == "investigating" &&
                progressed->at("phase") == "recovery" &&
                progressed->at("activeRunId") == "run_1" && !progressed->contains("completedAt"),
            "update_investigation_task_progress should write status, phase and active run");
      check(!database
                 .update_investigation_task_progress("itask_missing", "investigating", "recon",
                                                     std::nullopt, std::nullopt,
                                                     "2026-01-01T00:00:30Z", std::nullopt)
                 .has_value(),
            "update_investigation_task_progress should report an unknown task");

      const nlohmann::json plan = {
          {"id", "iplan_write"},
          {"taskId", "itask_write"},
          {"revision", 1},
          {"status", "active"},
          {"createdAt", "2026-01-01T00:00:30Z"},
          {"updatedAt", "2026-01-01T00:00:30Z"},
          {"currentStepId", "step_collect"},
          {"approval", {{"status", "pending"}}},
          {"steps",
           nlohmann::json::array(
               {{{"id", "step_collect"},
                 {"ordinal", 0},
                 {"kind", "evidence"},
                 {"title", "Collect config"},
                 {"status", "pending"},
                 {"attempts", 0},
                 {"maxAttempts", 1},
                 {"allowedTools", nlohmann::json::array({"filesystem.read"})}}})},
      };
      database.save_investigation_plan(plan);
      const auto latest = database.latest_investigation_plan("itask_write");
      check(latest.has_value() && latest->at("id") == "iplan_write" &&
                latest->at("revision") == 1 && latest->at("status") == "active" &&
                latest->at("currentStepId") == "step_collect" &&
                latest->at("approval").at("status") == "pending" &&
                latest->at("steps").at(0).at("allowedTools").at(0) == "filesystem.read",
            "latest_investigation_plan should round-trip the saved plan");
      check(database.get_investigation_plan("iplan_write") == latest,
            "get_investigation_plan should match latest_investigation_plan");
      check(!database.get_investigation_plan("iplan_missing").has_value(),
            "get_investigation_plan should report an unknown plan");

      auto second_plan = plan;
      second_plan["id"] = "iplan_write_2";
      second_plan["revision"] = 2;
      second_plan["createdAt"] = "2026-01-01T00:01:00Z";
      second_plan["updatedAt"] = "2026-01-01T00:01:00Z";
      database.save_investigation_plan(second_plan);
      check(database.latest_investigation_plan("itask_write")->at("id") == "iplan_write_2" &&
                database.get_investigation_plan("iplan_write")->at("status") == "superseded",
            "saving a new plan should supersede the previous active revision");

      const nlohmann::json artifact = {
          {"id", "iartifact_write"},
          {"taskId", "itask_write"},
          {"planId", "iplan_write_2"},
          {"planStepId", "step_collect"},
          {"phase", "investigating"},
          {"kind", "change_plan"},
          {"status", "ready"},
          {"title", "Pending change plan"},
          {"summary", "Saved"},
          {"content", second_plan},
          {"evidenceIds", nlohmann::json::array({"iev_write"})},
          {"createdAt", "2026-01-01T00:01:00Z"},
          {"updatedAt", "2026-01-01T00:01:00Z"},
      };
      database.upsert_investigation_artifact(artifact);
      const auto artifacts = database.list_investigation_artifacts("itask_write", 10);
      check(artifacts.at("artifacts").size() == 1 &&
                artifacts.at("artifacts").at(0).at("kind") == "change_plan" &&
                artifacts.at("artifacts").at(0).at("planId") == "iplan_write_2" &&
                artifacts.at("artifacts").at(0).at("planStepId") == "step_collect" &&
                artifacts.at("artifacts").at(0).at("content").at("id") == "iplan_write_2",
            "list_investigation_artifacts should round-trip the plan artifact");

      const nlohmann::json evidence = {
          {"id", "iev_write"},
          {"taskId", "itask_write"},
          {"scope",
           {{"host", "remote"}, {"serverId", "srv_01abc"}, {"environment", "staging"}}},
          {"kind", "file"},
          {"sourceTool", "filesystem.read"},
          {"collectedAt", "2026-01-01T00:02:00Z"},
          {"inputSummary", "read nginx.conf"},
          {"contentType", "json"},
          {"content", content},
          {"contentHash", content_hash},
          {"truncated", false},
          {"redactionStatus", "redacted"},
      };
      database.record_investigation_evidence(evidence);
      const auto stored_evidence = database.get_investigation_evidence("iev_write");
      check(stored_evidence.has_value() && stored_evidence->at("content") == content &&
                stored_evidence->at("contentHash") == content_hash &&
                !stored_evidence->contains("runId") &&
                stored_evidence->at("truncated") == false,
            "get_investigation_evidence should round-trip the persisted content");
      const auto deduped = database.find_investigation_evidence_in_run(evidence);
      check(!deduped.has_value(),
            "find_investigation_evidence_in_run should ignore evidence without a run id");
      auto run_evidence = evidence;
      run_evidence["id"] = "iev_run";
      run_evidence["sourceTool"] = "filesystem.stat";
      run_evidence["runId"] = "run_1";
      database.record_investigation_evidence(run_evidence);
      const auto matched = database.find_investigation_evidence_in_run(run_evidence);
      check(matched.has_value() && matched->at("id") == "iev_run" &&
                matched->at("runId") == "run_1",
            "find_investigation_evidence_in_run should match an identical run record");
      const auto search = database.search_investigation_evidence(
          "itask_write", {{"sourceTool", "filesystem.read"}, {"limit", 10}});
      check(search.is_array() && search.size() == 1 && search.at(0).at("id") == "iev_write",
            "search_investigation_evidence should filter by source tool");
      check(database
                .search_investigation_evidence("itask_write",
                                               {{"sourceTool", "process.list"}, {"limit", 10}})
                .empty(),
            "search_investigation_evidence should exclude other source tools");

      bool hash_mismatch_rejected = false;
      try {
        auto tampered = evidence;
        tampered["id"] = "iev_tampered";
        tampered["contentHash"] = std::string(64, '0');
        database.record_investigation_evidence(tampered);
      } catch (const std::invalid_argument&) {
        hash_mismatch_rejected = true;
      }
      check(hash_mismatch_rejected,
            "record_investigation_evidence should reject a mismatched content hash");

      bool unredacted_rejected = false;
      try {
        auto unredacted = evidence;
        unredacted["id"] = "iev_unredacted";
        unredacted["redactionStatus"] = "unknown";
        database.record_investigation_evidence(unredacted);
      } catch (const std::invalid_argument&) {
        unredacted_rejected = true;
      }
      check(unredacted_rejected,
            "record_investigation_evidence should reject unredacted content");

      const nlohmann::json finding = {
          {"id", "ifind_write"},
          {"taskId", "itask_write"},
          {"title", "nginx active"},
          {"kind", "fact"},
          {"statement", "nginx is active"},
          {"evidenceIds", nlohmann::json::array({"iev_write"})},
          {"confidence", "high"},
          {"createdAt", "2026-01-01T00:03:00Z"},
      };
      database.record_investigation_finding(finding);
      const auto stored_finding = database.get_investigation_finding("ifind_write");
      check(stored_finding.has_value() && stored_finding->at("title") == "nginx active" &&
                stored_finding->at("evidenceIds").at(0) == "iev_write" &&
                !stored_finding->contains("nextVerification"),
            "record_investigation_finding should round-trip the shared finding contract");

      const nlohmann::json brief = {
          {"id", "ibrief_write"},
          {"taskId", "itask_write"},
          {"planId", "iplan_write_2"},
          {"generatedAt", "2026-01-01T00:04:00Z"},
          {"status", "draft"},
          {"findingIds", nlohmann::json::array({"ifind_write"})},
          {"options",
           nlohmann::json::array(
               {{{"id", "opt_restart"}, {"title", "Restart nginx"}, {"status", "proposed"}},
                {{"id", "opt_wait"}, {"title", "Wait"}, {"status", "proposed"}}})},
      };
      database.record_investigation_brief(brief);
      const auto stored_brief = database.get_investigation_brief("ibrief_write");
      check(stored_brief.has_value() && stored_brief->at("status") == "draft" &&
                stored_brief->at("options").size() == 2 &&
                !stored_brief->contains("selectedOptionId"),
            "record_investigation_brief should round-trip the decision brief");
      const auto selected = database.select_investigation_brief_option(
          "itask_write", "ibrief_write", "opt_restart");
      check(selected.has_value() && selected->at("status") == "selected" &&
                selected->at("selectedOptionId") == "opt_restart" &&
                selected->at("options").at(0).at("status") == "selected" &&
                selected->at("options").at(1).at("status") == "rejected",
            "select_investigation_brief_option should select one option and reject the rest");
      bool wrong_task_rejected = false;
      try {
        (void)database.select_investigation_brief_option("itask_1", "ibrief_write", "opt_wait");
      } catch (const std::invalid_argument&) {
        wrong_task_rejected = true;
      }
      check(wrong_task_rejected,
            "select_investigation_brief_option should reject a brief from another task");

      // The Agent→host RPC entry point dispatches to the same durable store and
      // keeps enforcing the scope, hash and plan guards.
      const auto recorded_plan = router.handle_host_request(
          "host.investigation.plan.record", {{"plan", second_plan}});
      check(recorded_plan.value("recorded", false) == true &&
                recorded_plan.at("plan").at("id") == "iplan_write_2" &&
                recorded_plan.at("plan").at("status") == "active",
            "host.investigation.plan.record should persist a plan through the host RPC");

      const nlohmann::json host_content = {{"note", "host-recorded"}};
      const nlohmann::json host_evidence = {
          {"id", "iev_host"},
          {"taskId", "itask_write"},
          {"scope",
           {{"host", "remote"}, {"serverId", "srv_01abc"}, {"environment", "staging"}}},
          {"kind", "tool_result"},
          {"sourceTool", "filesystem.read"},
          {"collectedAt", "2026-01-01T00:05:00Z"},
          {"inputSummary", "host read"},
          {"contentType", "json"},
          {"content", host_content},
          {"contentHash", DesktopDatabase::content_hash(host_content)},
          {"truncated", false},
          {"redactionStatus", "redacted"},
      };
      const auto recorded_evidence = router.handle_host_request(
          "host.investigation.evidence.record", {{"evidence", host_evidence}});
      check(recorded_evidence.value("recorded", false) == true &&
                recorded_evidence.value("reused", true) == false &&
                recorded_evidence.value("evidenceId", std::string()) == "iev_host",
            "host.investigation.evidence.record should record new evidence");
      const auto reused_evidence = router.handle_host_request(
          "host.investigation.evidence.record", {{"evidence", host_evidence}});
      check(reused_evidence.value("recorded", false) == true &&
                reused_evidence.value("reused", false) == true &&
                reused_evidence.value("evidenceId", std::string()) == "iev_host",
            "host.investigation.evidence.record should reuse identical run evidence");

      const auto out_of_scope = router.handle_host_request(
          "host.investigation.evidence.record",
          {{"evidence",
            {{"id", "iev_out_of_scope"},
             {"taskId", "itask_write"},
             {"scope", {{"host", "remote"}, {"serverId", "srv_other"}}},
             {"kind", "file"},
             {"sourceTool", "filesystem.read"},
             {"collectedAt", "2026-01-01T00:02:00Z"},
             {"inputSummary", "x"},
             {"contentType", "json"},
             {"content", content},
             {"contentHash", content_hash},
             {"truncated", false},
             {"redactionStatus", "redacted"}}}});
      check(out_of_scope.value("recorded", true) == false &&
                out_of_scope.at("error").at("code") == "denied_by_policy",
            "host.investigation.evidence.record should deny an out-of-scope target");

      // Body-free comparison of two persisted observations, with host freshness.
      const nlohmann::json cmp_scope = {
          {"host", "remote"}, {"serverId", "srv_01abc"}, {"environment", "staging"}};
      const auto make_cmp_evidence = [&](const std::string& id, const nlohmann::json& body) {
        return nlohmann::json{
            {"id", id},
            {"taskId", "itask_write"},
            {"scope", cmp_scope},
            {"kind", "tool_result"},
            {"sourceTool", "filesystem.read"},
            {"collectedAt", "2026-01-01T00:08:00Z"},
            {"inputSummary", "compare fixture"},
            {"contentType", "json"},
            {"content", body},
            {"contentHash", DesktopDatabase::content_hash(body)},
            {"truncated", false},
            {"redactionStatus", "redacted"}};
      };
      database.record_investigation_evidence(
          make_cmp_evidence("iev_cmp_left", {{"value", 1}, {"note", "same"}}));
      database.record_investigation_evidence(
          make_cmp_evidence("iev_cmp_right", {{"value", 2}, {"note", "same"}}));
      const auto comparison = router.handle_host_request(
          "host.investigation.evidence.compare",
          {{"taskId", "itask_write"},
           {"leftEvidenceId", "iev_cmp_left"},
           {"rightEvidenceId", "iev_cmp_right"}});
      check(comparison.value("status", std::string()) == "success" &&
                comparison.at("comparison").at("status") == "changed" &&
                comparison.at("comparison").at("shape") == "json" &&
                comparison.at("comparison").at("diffTruncated") == false &&
                comparison.at("comparison").at("changedPathCount").get<int>() >= 1 &&
                comparison.at("comparison").at("changedPaths").size() >= 1 &&
                comparison.at("comparison").at("left").at("freshness").at("policy") ==
                    "default-v1" &&
                !comparison.at("comparison").at("left").contains("content"),
            "host.investigation.evidence.compare should report changed JSON paths without bodies");
      const auto identical = router.handle_host_request(
          "host.investigation.evidence.compare",
          {{"taskId", "itask_write"},
           {"leftEvidenceId", "iev_cmp_left"},
           {"rightEvidenceId", "iev_cmp_left"}});
      check(identical.at("comparison").at("status") == "identical" &&
                identical.at("comparison").at("changedPaths").empty(),
            "compare should report two identical observations as identical");
      const auto compare_unknown_task = router.handle_host_request(
          "host.investigation.evidence.compare",
          {{"taskId", "itask_missing"},
           {"leftEvidenceId", "iev_cmp_left"},
           {"rightEvidenceId", "iev_cmp_right"}});
      check(compare_unknown_task.at("error").at("code") == "not_found",
            "compare should report an unknown task");
      const auto compare_foreign = router.handle_host_request(
          "host.investigation.evidence.compare",
          {{"taskId", "itask_created"},
           {"leftEvidenceId", "iev_cmp_left"},
           {"rightEvidenceId", "iev_cmp_right"}});
      check(compare_foreign.at("error").at("code") == "denied_by_policy",
            "compare should deny evidence from another task scope");

      const auto correlation = router.handle_host_request(
          "host.investigation.evidence.correlate",
          {{"taskId", "itask_write"}, {"anchorEvidenceId", "iev_cmp_left"}});
      check(correlation.value("status", std::string()) == "success" &&
                correlation.at("correlation").at("matchedBy") == "time_window" &&
                correlation.at("correlation").at("windowSeconds") == 300 &&
                correlation.at("correlation").at("anchor").at("id") == "iev_cmp_left" &&
                !correlation.at("correlation").at("evidence").empty() &&
                correlation.at("correlation").at("sourceTools").at(0) == "filesystem.read",
            "host.investigation.evidence.correlate should group by the time window");
      const auto same_run_correlation = router.handle_host_request(
          "host.investigation.evidence.correlate",
          {{"taskId", "itask_write"}, {"anchorEvidenceId", "iev_run"}});
      check(same_run_correlation.at("correlation").at("matchedBy") == "same_run" &&
                same_run_correlation.at("correlation").at("anchor").at("runId") == "run_1",
            "correlate should prefer the host-owned run relation");
      const auto bad_window = router.handle_host_request(
          "host.investigation.evidence.correlate",
          {{"taskId", "itask_write"},
           {"anchorEvidenceId", "iev_cmp_left"},
           {"windowSeconds", 0}});
      check(bad_window.at("error").at("code") == "invalid_input",
            "correlate should bound the correlation window");

      const auto retention_missing = router.handle_host_request(
          "host.investigation.retention.preview", {{"taskId", "itask_missing"}});
      check(retention_missing.value("status", std::string()) == "failed" &&
                retention_missing.at("error").at("code") == "not_found",
            "the host retention preview should report an unknown task");
      const auto retention_bad_limit = router.handle_host_request(
          "host.investigation.retention.preview", {{"taskId", "itask_write"}, {"limit", 0}});
      check(retention_bad_limit.at("error").at("code") == "invalid_input",
            "the host retention preview should reject an out-of-range limit");

      const auto deviation = router.handle_host_request(
          "host.investigation.plan.check",
          {{"taskId", "itask_write"},
           {"toolName", "server.exec"},
           {"target", {{"host", "remote"}, {"serverId", "srv_01abc"}}},
           {"input", nlohmann::json::object()}});
      check(deviation.value("status", std::string()) == "deviation" &&
                deviation.at("deviation").at("code") == "tool_not_allowed",
            "host.investigation.plan.check should report a disallowed tool as a deviation");

      bool unknown_rejected = false;
      try {
        (void)router.handle_host_request("host.investigation.bogus", nlohmann::json::object());
      } catch (const yukinal::desktop_host::SidecarRpcError& error) {
        unknown_rejected = error.code() == -32601;
      }
      check(unknown_rejected,
            "the host RPC entry point should reject an unknown investigation method");
    }

    // ── investigation_brief_select ───────────────────────────────────────────
    // Selecting a decision option returns its continuation, and only an explicit
    // start_plan continuation approves the linked dry-run plan.
    {
      const nlohmann::json select_brief = {
          {"id", "ibrief_select"},
          {"taskId", "itask_write"},
          {"planId", "iplan_write_2"},
          {"generatedAt", "2026-01-01T00:06:00Z"},
          {"status", "draft"},
          {"findingIds", nlohmann::json::array({"ifind_write"})},
          {"options",
           nlohmann::json::array(
               {{{"id", "opt_start"},
                 {"title", "Start"},
                 {"summary", "run the plan"},
                 {"impact", "writes files"},
                 {"riskLevel", "medium"},
                 {"evidenceIds", nlohmann::json::array()},
                 {"findingIds", nlohmann::json::array()},
                 {"verification", "check the service"},
                 {"requiresApproval", true},
                 {"status", "proposed"},
                 {"continuation", "start_plan"}},
                {{"id", "opt_readonly"},
                 {"title", "Keep reading"},
                 {"summary", "read only"},
                 {"impact", "none"},
                 {"riskLevel", "low"},
                 {"evidenceIds", nlohmann::json::array()},
                 {"findingIds", nlohmann::json::array()},
                 {"verification", "check the service"},
                 {"requiresApproval", false},
                 {"status", "proposed"},
                 {"continuation", "continue_readonly"}},
                {{"id", "opt_stop"},
                 {"title", "Stop"},
                 {"summary", "stop the task"},
                 {"impact", "none"},
                 {"riskLevel", "low"},
                 {"evidenceIds", nlohmann::json::array()},
                 {"findingIds", nlohmann::json::array()},
                 {"verification", "check the service"},
                 {"requiresApproval", false},
                 {"status", "proposed"},
                 {"continuation", "stop"}}})},
      };
      database.record_investigation_brief(select_brief);

      auto select_option = [&router](const char* option_id) {
        return router.invoke(
            "investigation_brief_select",
            {{"input",
              {{"taskId", "itask_write"}, {"briefId", "ibrief_select"}, {"optionId", option_id}}}});
      };
      const auto start_plan = select_option("opt_start");
      check(start_plan.ok && start_plan.value.at("continuation") == "start_plan" &&
                start_plan.value.at("brief").at("status") == "selected" &&
                start_plan.value.at("brief").at("selectedOptionId") == "opt_start",
            "investigation_brief_select should return the start_plan continuation");
      const auto approved_plan = database.get_investigation_plan("iplan_write_2");
      check(approved_plan.has_value() &&
                approved_plan->at("approval").at("status") == "approved" &&
                approved_plan->at("approval").at("source") == "user" &&
                approved_plan->at("approval").at("optionId") == "opt_start",
            "a start_plan continuation should approve the linked plan for the selecting user");

      // A read-only continuation must not approve the plan that produced the brief.
      auto reset_plan = database.get_investigation_plan("iplan_write_2");
      (*reset_plan)["approval"] = {{"status", "pending"}};
      database.save_investigation_plan(*reset_plan);
      const auto readonly_choice = select_option("opt_readonly");
      check(readonly_choice.ok &&
                readonly_choice.value.at("continuation") == "continue_readonly" &&
                database.get_investigation_plan("iplan_write_2")->at("approval").at("status") ==
                    "pending",
            "a read-only continuation must leave the linked plan unapproved");

      check(!router
                 .invoke("investigation_brief_select",
                         {{"input",
                           {{"taskId", "itask_write"},
                            {"briefId", "ibrief_select"},
                            {"optionId", "opt_missing"}}}})
                 .ok,
            "investigation_brief_select should reject an unknown option");
      check(!router
                 .invoke("investigation_brief_select",
                         {{"input",
                           {{"taskId", "itask_write"},
                            {"briefId", "ibrief_missing"},
                            {"optionId", "opt_start"}}}})
                 .ok,
            "investigation_brief_select should report an unknown brief");
      check(!router
                 .invoke("investigation_brief_select",
                         {{"input",
                           {{"taskId", "itask_1"},
                            {"briefId", "ibrief_select"},
                            {"optionId", "opt_stop"}}}})
                 .ok,
            "investigation_brief_select should refuse a terminal task");
      check(!router.invoke("investigation_brief_select", nlohmann::json::object()).ok,
            "investigation_brief_select requires an input object");

      // A stop continuation terminates the task.
      const auto stop_choice = select_option("opt_stop");
      check(stop_choice.ok && stop_choice.value.at("continuation") == "stop" &&
                database.get_investigation_task("itask_write")->at("task").at("status") ==
                    "stopped",
            "a stop continuation should terminate the task");
    }

    // ── investigation_retention_preview / prune ─────────────────────────────
    // Retention is a two-step, reference-aware cleanup of terminal task history.
    {
      const nlohmann::json ret_content = {{"note", "retention"}};
      const auto ret_hash = DesktopDatabase::content_hash(ret_content);
      const nlohmann::json base_evidence = {
          {"taskId", "itask_write"},
          {"scope", {{"host", "remote"}, {"serverId", "srv_01abc"}, {"environment", "staging"}}},
          {"kind", "file"},
          {"sourceTool", "filesystem.read"},
          {"inputSummary", "retention fixture"},
          {"contentType", "json"},
          {"content", ret_content},
          {"contentHash", ret_hash},
          {"truncated", false},
          {"redactionStatus", "redacted"},
      };
      auto drop_evidence = base_evidence;
      drop_evidence["id"] = "iev_ret_drop";
      drop_evidence["collectedAt"] = "2025-12-01T00:00:00Z";
      database.record_investigation_evidence(drop_evidence);
      auto keep_evidence = base_evidence;
      keep_evidence["id"] = "iev_ret_keep";
      keep_evidence["collectedAt"] = "2025-12-02T00:00:00Z";
      database.record_investigation_evidence(keep_evidence);

      // Evidence referenced by a finding is protected from retention.
      database.record_investigation_finding({
          {"id", "ifind_ret"},
          {"taskId", "itask_write"},
          {"title", "retention keep"},
          {"kind", "fact"},
          {"statement", "keep this evidence"},
          {"evidenceIds", nlohmann::json::array({"iev_ret_keep"})},
          {"confidence", "high"},
          {"createdAt", "2026-01-01T00:07:00Z"},
      });

      const auto make_artifact = [](const std::string& id, const std::string& status,
                                    const std::string& updated) {
        return nlohmann::json{
            {"id", id},
            {"taskId", "itask_write"},
            {"phase", "investigating"},
            {"kind", "change_plan"},
            {"status", status},
            {"title", "retention artifact"},
            {"summary", "retention artifact"},
            {"content", {{"note", "artifact"}}},
            {"evidenceIds", nlohmann::json::array()},
            {"createdAt", "2025-11-01T00:00:00Z"},
            {"updatedAt", updated},
        };
      };
      database.upsert_investigation_artifact(
          make_artifact("iart_ret_drop", "superseded", "2025-12-05T00:00:00Z"));
      database.upsert_investigation_artifact(
          make_artifact("iart_ret_live", "ready", "2025-12-06T00:00:00Z"));

      const std::string cutoff = "2026-06-01T00:00:00Z";
      const auto contains_id = [](const nlohmann::json& array, const std::string& id) {
        for (const auto& entry : array) {
          if (entry.value("id", std::string()) == id) return true;
        }
        return false;
      };
      const auto preview = router.invoke(
          "investigation_retention_preview", {{"taskId", "itask_write"}, {"cutoffAt", cutoff}});
      check(preview.ok && preview.value.at("preview").at("taskId") == "itask_write" &&
                preview.value.at("preview").at("cutoffAt") == cutoff &&
                preview.value.at("preview").at("truncated") == false &&
                contains_id(preview.value.at("preview").at("candidates"), "iev_ret_drop") &&
                contains_id(preview.value.at("preview").at("candidates"), "iart_ret_drop") &&
                !contains_id(preview.value.at("preview").at("candidates"), "iev_ret_keep") &&
                !contains_id(preview.value.at("preview").at("candidates"), "iart_ret_live") &&
                preview.value.at("preview").at("protectedCount").get<int>() >= 2 &&
                preview.value.at("preview").at("candidateBytes").get<int>() > 0,
            "investigation_retention_preview should report only unreferenced old history");

      const auto prune = router.invoke(
          "investigation_retention_prune",
          {{"input",
            {{"taskId", "itask_write"},
             {"cutoffAt", cutoff},
             {"confirmation", "delete_unreferenced"},
             {"items", nlohmann::json::array(
                 {{{"id", "iev_ret_drop"}, {"kind", "evidence"}},
                  {{"id", "iev_ret_keep"}, {"kind", "evidence"}},
                  {{"id", "iart_ret_drop"}, {"kind", "artifact"}},
                  {{"id", "iev_ret_missing"}, {"kind", "evidence"}}})}}}});
      check(prune.ok && contains_id(prune.value.at("result").at("deleted"), "iev_ret_drop") &&
                contains_id(prune.value.at("result").at("deleted"), "iart_ret_drop") &&
                contains_id(prune.value.at("result").at("skipped"), "iev_ret_keep") &&
                contains_id(prune.value.at("result").at("skipped"), "iev_ret_missing"),
            "investigation_retention_prune should delete only the named unreferenced items");
      check(!database.get_investigation_evidence("iev_ret_drop").has_value() &&
                database.get_investigation_evidence("iev_ret_keep").has_value(),
            "prune should remove the dropped evidence and keep referenced evidence");
      const auto artifacts_after = database.list_investigation_artifacts("itask_write", 32);
      check(!contains_id(artifacts_after.at("artifacts"), "iart_ret_drop") &&
                contains_id(artifacts_after.at("artifacts"), "iart_ret_live"),
            "prune should remove only superseded artifacts");

      bool audit_recorded = false;
      // Bind the response to a named value: iterating over `.at()` of a temporary
      // JSON would iterate a dangling reference (no lifetime extension).
      const auto audit_activities = database.list_activities(std::nullopt, 20);
      for (const auto& activity : audit_activities.at("activities")) {
        if (activity.value("type", std::string()) == "agent_action" &&
            activity.value("description", std::string())
                    .find("retention prune task=itask_write") != std::string::npos) {
          audit_recorded = true;
        }
      }
      check(audit_recorded, "retention prune should record an audit activity");

      check(!router
                 .invoke(
                     "investigation_retention_prune",
                     {{"input",
                       {{"taskId", "itask_write"},
                        {"cutoffAt", cutoff},
                        {"confirmation", "nope"},
                        {"items", nlohmann::json::array(
                                      {{{"id", "iev_ret_keep"}, {"kind", "evidence"}}})}}}})
                 .ok,
            "retention prune requires the explicit confirmation literal");
      check(!router.invoke("investigation_retention_preview", {{"taskId", "itask_created"}}).ok,
            "retention is available only for terminal tasks");
      check(!router.invoke("investigation_retention_preview", {{"taskId", "itask_missing"}}).ok,
            "retention preview should report an unknown task");
      check(!router
                 .invoke("investigation_retention_preview",
                         {{"taskId", "itask_write"}, {"limit", 0}})
                 .ok,
            "retention preview should reject an out-of-range limit");
      check(router.invoke("investigation_retention_preview", {{"taskId", "itask_write"}}).ok,
            "retention preview should default its cutoff and limit");

      // The Agent-facing host RPC reuses the same durable preview without a
      // delete capability.
      const auto host_preview = router.handle_host_request(
          "host.investigation.retention.preview",
          {{"taskId", "itask_write"}, {"cutoffAt", cutoff}});
      check(host_preview.value("status", std::string()) == "success" &&
                host_preview.at("preview").at("taskId") == "itask_write" &&
                host_preview.at("preview").at("candidates").is_array() &&
                !contains_id(host_preview.at("preview").at("candidates"), "iart_ret_live"),
            "host.investigation.retention.preview should expose host retention candidates");
    }

    // ── host.context.fetch ───────────────────────────────────────────────────
    // Read-only metadata projection for sidecar context assembly: bodies never
    // travel, and unknown kinds or malformed ids are refused before any lookup.
    {
      const auto carries_body = [](const nlohmann::json& array) {
        for (const auto& entry : array) {
          if (entry.contains("content")) return true;
        }
        return false;
      };
      const auto context = router.handle_host_request(
          "host.context.fetch", {{"kind", "investigation"}, {"id", "itask_write"}});
      check(context.value("status", std::string()) == "success" &&
                context.at("data").at("task").at("id") == "itask_write" &&
                context.at("data").at("evidence").is_array() &&
                !context.at("data").at("evidence").empty() &&
                !carries_body(context.at("data").at("evidence")) &&
                !carries_body(context.at("data").at("artifacts")) &&
                context.at("data").at("findings").is_array() &&
                context.at("data").at("runs").is_array() &&
                context.at("data").at("steps").is_array() &&
                context.at("data").at("evidence").at(0).at("freshness").at("policy") ==
                    "default-v1",
            "host.context.fetch should project a body-free investigation context");

      const auto server_context = router.handle_host_request(
          "host.context.fetch", {{"kind", "server"}, {"id", "srv_01abc"}});
      check(server_context.value("status", std::string()) == "success" &&
                server_context.at("data").at("id") == "srv_01abc",
            "host.context.fetch should return one server by id");

      const auto snapshot_context = router.handle_host_request(
          "host.context.fetch", {{"kind", "snapshot"}, {"id", "srv_01abc"}});
      check(snapshot_context.value("status", std::string()) == "success" &&
                snapshot_context.at("data").at("id") == "snap_srv_01abc_1",
            "host.context.fetch should return the latest server snapshot");

      const auto workspace_context = router.handle_host_request(
          "host.context.fetch", {{"kind", "workspace"}, {"id", "ws_01"}});
      check(workspace_context.value("status", std::string()) == "success" &&
                workspace_context.at("data").at("id") == "ws_01",
            "host.context.fetch should return one workspace by id");

      const auto unknown_kind = router.handle_host_request(
          "host.context.fetch", {{"kind", "galaxy"}, {"id", "srv_01abc"}});
      check(unknown_kind.value("status", std::string()) == "failed" &&
                unknown_kind.at("error").at("code") == "invalid_input",
            "host.context.fetch should refuse an unknown context kind");

      const auto non_opaque_server = router.handle_host_request(
          "host.context.fetch", {{"kind", "server"}, {"id", "01abc"}});
      check(non_opaque_server.value("status", std::string()) == "failed" &&
                non_opaque_server.at("error").at("code") == "invalid_input",
            "host.context.fetch should require an opaque srv_ id for servers");

      const auto empty_id = router.handle_host_request(
          "host.context.fetch", {{"kind", "investigation"}, {"id", ""}});
      check(empty_id.value("status", std::string()) == "failed" &&
                empty_id.at("error").at("code") == "invalid_input",
            "host.context.fetch should require a non-empty context id");

      const auto missing_task = router.handle_host_request(
          "host.context.fetch", {{"kind", "investigation"}, {"id", "itask_missing"}});
      check(missing_task.value("status", std::string()) == "not_found",
            "host.context.fetch should report an unknown investigation as not found");

      const auto missing_snapshot = router.handle_host_request(
          "host.context.fetch", {{"kind", "snapshot"}, {"id", "srv_none"}});
      check(missing_snapshot.value("status", std::string()) == "not_found",
            "host.context.fetch should report a server without snapshots as not found");
    }

    // ── investigation_schedule_* ─────────────────────────────────────────────
    // Durable read-only triggers: create, list, update, runs, and the due-claiming
    // tick that funnels every trigger through one dedupe bucket.
    {
      const nlohmann::json readonly_task = {
          {"id", "itask_sched"},
          {"objective", "Poll nginx health"},
          {"successCriteria", nlohmann::json::array({"collect readonly samples"})},
          {"scope",
           {{"host", "remote"}, {"serverId", "srv_01abc"}, {"environment", "staging"}}},
          {"guardrails",
           {{"forbiddenTools", nlohmann::json::array()},
            {"forbiddenPathPrefixes", nlohmann::json::array()}}},
          {"mode", "readonly"},
          {"permissionMode", "ask"},
          {"automationLevel", "readonly"},
          {"phase", "investigating"},
          {"status", "pending"},
          {"budget", {{"maxSteps", 6}, {"maxRunMs", 60000}, {"maxAttempts", 2}}},
          {"createdBy", "user"},
          {"createdAt", "2026-01-02T00:00:00Z"},
          {"updatedAt", "2026-01-02T00:00:00Z"},
      };
      (void)database.create_investigation_task(readonly_task);

      const auto created = router.invoke(
          "investigation_schedule_create",
          {{"input",
            {{"taskId", "itask_sched"},
             {"intervalSeconds", 60},
             {"cooldownSeconds", 0},
             {"dedupeWindowSeconds", 60},
             {"notificationPolicy", "on_change"},
             {"nextRunAt", "2026-02-01T00:00:00Z"}}}});
      check(created.ok && created.value.at("schedule").at("taskId") == "itask_sched" &&
                created.value.at("schedule").at("status") == "active" &&
                created.value.at("schedule").at("intervalSeconds") == 60 &&
                created.value.at("schedule").at("maxConcurrentRuns") == 1 &&
                created.value.at("schedule").at("dedupeWindowSeconds") == 60 &&
                created.value.at("schedule").at("budget").at("maxSteps") == 6 &&
                created.value.at("schedule").at("budget").at("maxAttempts") == 2 &&
                created.value.at("schedule").at("notificationPolicy") == "on_change",
            "schedule create should default the optional bounds from the task envelope");
      const std::string schedule_id =
          created.value.at("schedule").at("id").get<std::string>();

      const auto listed = router.invoke("investigation_schedule_list", nlohmann::json::object());
      check(listed.ok && listed.value.at("schedules").size() == 1 &&
                listed.value.at("schedules").at(0).at("id") == schedule_id,
            "schedule list should expose the durable trigger");
      check(!router.invoke("investigation_schedule_list", {{"unexpected", 1}}).ok,
            "investigation_schedule_list does not accept parameters");

      // A due read-only trigger is claimed exactly once for its dedupe bucket.
      const auto tick = router.invoke(
          "investigation_schedule_tick", {{"input", {{"now", "2026-02-01T00:00:01Z"}}}});
      check(tick.ok && tick.value.at("claimed").size() == 1 &&
                tick.value.at("claimed").at(0).at("scheduleId") == schedule_id &&
                tick.value.at("claimed").at(0).at("taskId") == "itask_sched" &&
                tick.value.at("claimed").at(0).at("status") == "claimed" &&
                tick.value.at("skipped").empty(),
            "a due read-only trigger should be claimed exactly once");
      const auto repeat = router.invoke(
          "investigation_schedule_tick", {{"input", {{"now", "2026-02-01T00:00:01Z"}}}});
      check(repeat.ok && repeat.value.at("claimed").empty() &&
                repeat.value.at("skipped").empty(),
            "a second tick at the same instant must not fabricate a run");

      // The in-flight claimed run makes the next bucket skip on the concurrency cap.
      const auto limited = router.invoke(
          "investigation_schedule_tick", {{"input", {{"now", "2026-02-01T00:01:02Z"}}}});
      check(limited.ok && limited.value.at("claimed").empty() &&
                limited.value.at("skipped").size() == 1 &&
                limited.value.at("skipped").at(0).at("outcome") == "concurrency_limit" &&
                limited.value.at("skipped").at(0).at("status") == "skipped",
            "an in-flight run should make the next tick skip on the concurrency limit");

      const auto runs =
          router.invoke("investigation_schedule_runs", {{"scheduleId", schedule_id}});
      check(runs.ok && runs.value.at("runs").size() == 2 &&
                runs.value.at("runs").at(0).at("status") == "skipped" &&
                runs.value.at("runs").at(1).at("status") == "claimed" &&
                !runs.value.at("runs").at(0).at("dedupeKey").get<std::string>().empty(),
            "schedule runs should expose the claimed and skipped attempts newest first");
      check(!router.invoke("investigation_schedule_runs", nlohmann::json::object()).ok,
            "investigation_schedule_runs requires a scheduleId");

      // The last skipped outcome is persisted on the trigger.
      const auto after_skip =
          router.invoke("investigation_schedule_list", nlohmann::json::object());
      check(after_skip.value.at("schedules").at(0).at("lastOutcome") == "concurrency_limit" &&
                after_skip.value.at("schedules").at(0).at("lastRunAt").is_string(),
            "a skipped trigger should record its terminal outcome");

      const auto paused = router.invoke(
          "investigation_schedule_update",
          {{"input", {{"scheduleId", schedule_id}, {"status", "paused"}, {"intervalSeconds", 120}}}});
      check(paused.ok && paused.value.at("schedule").at("status") == "paused" &&
                paused.value.at("schedule").at("intervalSeconds") == 120,
            "schedule update should apply bounded changes");
      const auto revoked = router.invoke(
          "investigation_schedule_update",
          {{"input", {{"scheduleId", schedule_id}, {"status", "revoked"}}}});
      check(revoked.ok && revoked.value.at("schedule").at("status") == "revoked",
            "a schedule can be revoked");
      check(!router
                 .invoke("investigation_schedule_update",
                         {{"input", {{"scheduleId", schedule_id}, {"status", "active"}}}})
                 .ok,
            "revoked schedules cannot be reactivated");
      const auto after_revoke = router.invoke(
          "investigation_schedule_tick", {{"input", {{"now", "2026-02-01T05:00:00Z"}}}});
      check(after_revoke.ok && after_revoke.value.at("claimed").empty() &&
                after_revoke.value.at("skipped").empty(),
            "a revoked trigger must not be claimed");

      // Baseline selection requires an existing succeeded run that carries evidence.
      check(!router
                 .invoke("investigation_schedule_update",
                         {{"input",
                           {{"scheduleId", schedule_id}, {"baselineRunId", "schedrun_missing"}}}})
                 .ok,
            "baseline selection must reference an existing schedule run");

      // A trigger whose read-only task reaches a terminal status revokes itself on
      // the next due tick instead of launching anything.
      nlohmann::json terminal_task = readonly_task;
      terminal_task["id"] = "itask_sched_term";
      (void)database.create_investigation_task(terminal_task);
      const auto term_created = router.invoke(
          "investigation_schedule_create",
          {{"input",
            {{"taskId", "itask_sched_term"},
             {"intervalSeconds", 60},
             {"cooldownSeconds", 0},
             {"nextRunAt", "2026-03-01T00:00:00Z"}}}});
      check(term_created.ok, "a second read-only trigger should be creatable");
      const std::string term_id = term_created.value.at("schedule").at("id").get<std::string>();
      check(database
                .set_investigation_task_status("itask_sched_term", "completed",
                                               std::optional<std::string>(
                                                   "2026-03-01T00:00:00Z"),
                                               "2026-03-01T00:00:00Z")
                .has_value(),
            "the scheduler fixture task should move to a terminal status");
      const auto term_tick = router.invoke(
          "investigation_schedule_tick", {{"input", {{"now", "2026-03-01T00:00:01Z"}}}});
      check(term_tick.ok && term_tick.value.at("skipped").size() == 1 &&
                term_tick.value.at("skipped").at(0).at("scheduleId") == term_id &&
                term_tick.value.at("skipped").at(0).at("outcome") == "task_terminal",
            "a trigger whose task is terminal should record a task_terminal skip");
      const auto term_state = database.get_investigation_schedule(term_id);
      check(term_state.has_value() && term_state->at("status") == "revoked",
            "a terminal task should revoke its trigger");

      // Rejected create inputs.
      check(!router
                 .invoke("investigation_schedule_create",
                         {{"input", {{"taskId", "itask_write"}, {"intervalSeconds", 60}}}})
                 .ok,
            "scheduler triggers may only target read-only tasks");
      check(!router
                 .invoke("investigation_schedule_create",
                         {{"input", {{"taskId", "itask_sched"}, {"intervalSeconds", 0}}}})
                 .ok,
            "a schedule interval must be positive");
      check(!router
                 .invoke("investigation_schedule_create",
                         {{"input", {{"taskId", "itask_missing"}, {"intervalSeconds", 60}}}})
                 .ok,
            "scheduling an unknown task should fail");
      check(!router
                 .invoke("investigation_schedule_create",
                         {{"input",
                           {{"taskId", "itask_sched"},
                            {"intervalSeconds", 60},
                            {"budget", {{"maxSteps", 0}, {"maxRunMs", 1}, {"maxAttempts", 1}}}}}})
                 .ok,
            "a schedule budget must stay inside the supported bounds");
      check(!router.invoke("investigation_schedule_tick", {{"input", {{"limit", 0}}}}).ok,
            "a schedule tick limit must be between 1 and 100");
    }

    // ── investigation run binding + task recover ─────────────────────────────
    // The durable run row and the recovery path are the DB-only half of
    // `investigation_task_start` / `investigation_task_recover`; the sidecar
    // binding itself is covered by the sidecar test binary.
    {
      const std::string rec_now = "2026-01-04T00:00:00Z";
      const nlohmann::json recover_task = {
          {"id", "itask_recover"},
          {"objective", "Recover the interrupted service check"},
          {"successCriteria", nlohmann::json::array({"record the recovered state"})},
          {"scope", {{"host", "local"}, {"environment", "local"}}},
          {"guardrails",
           {{"forbiddenTools", nlohmann::json::array()},
            {"forbiddenPathPrefixes", nlohmann::json::array()}}},
          {"mode", "goal"},
          {"permissionMode", "ask"},
          {"automationLevel", "propose"},
          {"phase", "investigating"},
          {"status", "pending"},
          {"budget", {{"maxSteps", 5}, {"maxRunMs", 60000}, {"maxAttempts", 4}}},
          {"createdBy", "user"},
          {"createdAt", rec_now},
          {"updatedAt", rec_now},
      };
      (void)database.create_investigation_task(recover_task);

      const nlohmann::json run = {
          {"id", "run_itask_recover_1"},
          {"taskId", "itask_recover"},
          {"sessionId", "session_task_itask_recover"},
          {"messageId", "message_run_itask_recover_1"},
          {"attempt", 1},
          {"phase", "investigating"},
          {"status", "admitted"},
          {"startedAt", rec_now},
          {"updatedAt", rec_now}};
      database.create_investigation_run(run);
      const auto fetched_run = database.get_investigation_run("run_itask_recover_1");
      check(fetched_run.has_value() && fetched_run->at("taskId") == "itask_recover" &&
                fetched_run->at("status") == "admitted" && fetched_run->at("attempt") == 1 &&
                fetched_run->at("messageId") == "message_run_itask_recover_1",
            "a durable investigation run should round-trip through the database");
      check(!database.get_investigation_run("run_missing").has_value(),
            "get_investigation_run should report an unknown run");
      const auto runs = database.list_investigation_runs("itask_recover", 8);
      check(runs.is_array() && runs.size() == 1 &&
                runs.at(0).at("id") == "run_itask_recover_1",
            "list_investigation_runs should return the task's attempts");

      auto advanced_run = run;
      advanced_run["status"] = "running";
      advanced_run["updatedAt"] = "2026-01-04T00:00:03Z";
      const auto updated_run = database.update_investigation_run(advanced_run);
      check(updated_run.has_value() && updated_run->at("status") == "running" &&
                updated_run->at("updatedAt") == "2026-01-04T00:00:03Z",
            "update_investigation_run should persist the run progress");
      check(!database
                 .update_investigation_run(nlohmann::json{{"id", "run_missing"},
                                                          {"taskId", "itask_recover"},
                                                          {"attempt", 1},
                                                          {"phase", "investigating"},
                                                          {"status", "failed"},
                                                          {"startedAt", rec_now},
                                                          {"updatedAt", rec_now}})
                 .has_value(),
            "update_investigation_run should report an unknown run");

      const auto failure_options = nlohmann::json::array(
          {{{"id", "retry"},
            {"action", "retry"},
            {"title", "重试当前阶段"},
            {"description", "retry"},
            {"requiresApproval", false}},
           {{"id", "inspect"},
            {"action", "inspect"},
            {"title", "查看现场证据"},
            {"description", "inspect"},
            {"requiresApproval", false}},
           {{"id", "stop"},
            {"action", "stop"},
            {"title", "结束任务"},
            {"description", "stop"},
            {"requiresApproval", false}}});
      nlohmann::json failure_record = {{"code", "transport"},
                                       {"message", "sidecar run failed"},
                                       {"retryable", true},
                                       {"attempt", 1},
                                       {"at", "2026-01-04T00:00:05Z"},
                                       {"options", failure_options}};
      (void)database.update_investigation_task_progress("itask_recover", "failed", "recovery",
                                                        std::nullopt, failure_record,
                                                        "2026-01-04T00:00:05Z",
                                                        "2026-01-04T00:00:05Z");

      const nlohmann::json plan = {
          {"id", "iplan_recover"},
          {"taskId", "itask_recover"},
          {"revision", 1},
          {"status", "active"},
          {"createdAt", rec_now},
          {"updatedAt", rec_now},
          {"currentStepId", "step_recover"},
          {"approval", {{"status", "approved"}, {"source", "user"}, {"approvedAt", rec_now}}},
          {"observationWindow",
           {{"durationSeconds", 60},
            {"intervalSeconds", 10},
            {"allowedTools", nlohmann::json::array({"filesystem.read"})},
            {"successCriteria", nlohmann::json::array({"stable"})},
            {"status", "running"},
            {"sampleCount", 1}}},
          {"steps",
           nlohmann::json::array({{{"id", "step_recover"},
                                    {"ordinal", 0},
                                    {"kind", "evidence"},
                                    {"title", "Collect"},
                                    {"status", "running"},
                                    {"attempts", 1},
                                    {"allowedTools", nlohmann::json::array({"filesystem.read"})}}})},
      };
      database.save_investigation_plan(plan);
      const nlohmann::json baseline = {
          {"id", "iart_recover_baseline"},
          {"taskId", "itask_recover"},
          {"planId", "iplan_recover"},
          {"phase", "verification"},
          {"kind", "baseline"},
          {"status", "ready"},
          {"title", "Pre-change baseline"},
          {"summary", "Captured before the change"},
          {"content", {{"note", "baseline"}}},
          {"evidenceIds", nlohmann::json::array()},
          {"createdAt", rec_now},
          {"updatedAt", rec_now}};
      database.upsert_investigation_artifact(baseline);

      const auto recovered = router.invoke("investigation_task_recover",
                                           {{"input", {{"taskId", "itask_recover"}}}});
      check(recovered.ok && recovered.value.at("task").at("status") == "investigating" &&
                recovered.value.at("task").at("phase") == "recovery" &&
                !recovered.value.at("task").contains("activeRunId") &&
                recovered.value.at("task").at("lastFailure").at("code") == "transport" &&
                recovered.value.at("task").at("lastFailure").at("attempt") == 2 &&
                recovered.value.at("task").at("lastFailure").at("detail").at(
                    "requiresFreshBaseline") == true,
            "recover should resume the task into recovery with a fresh failure record");
      const auto invalidated_plan = database.latest_investigation_plan("itask_recover");
      check(invalidated_plan.has_value() &&
                invalidated_plan->at("approval").at("status") == "pending" &&
                invalidated_plan->at("observationWindow").at("status") == "cancelled",
            "recover should void the old plan approval and cancel the running observation window");
      const auto invalidated_artifacts =
          database.list_investigation_artifacts("itask_recover", 8);
      check(invalidated_artifacts.at("artifacts").size() == 1 &&
                invalidated_artifacts.at("artifacts").at(0).at("status") == "superseded",
            "recover should supersede the pre-change baseline artifact");
      const auto interrupted_run = database.get_investigation_run("run_itask_recover_1");
      check(interrupted_run.has_value() &&
                interrupted_run->at("status") == "interrupted" &&
                interrupted_run->at("failure").at("code") == "transport",
            "recover should interrupt the open run with the recovery failure");

      const auto inspected = router.invoke(
          "investigation_task_recover",
          {{"input", {{"taskId", "itask_recover"}, {"optionId", "inspect"}}}});
      check(inspected.ok && inspected.value.at("task").at("status") == "investigating",
            "the inspect recovery option should leave the task untouched");
      check(!router
                 .invoke("investigation_task_recover",
                         {{"input", {{"taskId", "itask_recover"}, {"optionId", "missing"}}}})
                 .ok,
            "recover should reject a failure option that is no longer offered");

      auto unknown_failure = failure_record;
      unknown_failure["code"] = "outcome_unknown";
      unknown_failure["retryable"] = false;
      (void)database.update_investigation_task_progress("itask_recover", "waiting_user", "recovery",
                                                        std::nullopt, unknown_failure,
                                                        "2026-01-04T00:00:06Z", std::nullopt);
      check(!router.invoke("investigation_task_recover", {{"input", {{"taskId", "itask_recover"}}}})
                 .ok,
            "an outcome-unknown failure requires an explicit recovery action");

      const auto stopped = router.invoke(
          "investigation_task_recover",
          {{"input", {{"taskId", "itask_recover"}, {"optionId", "stop"}}}});
      check(stopped.ok && stopped.value.at("task").at("status") == "stopped",
            "the stop recovery option should end the task");
      check(!router.invoke("investigation_task_start", {{"taskId", "itask_recover"}}).ok,
            "a stopped investigation task cannot start a new Agent run");
      check(!router.invoke("investigation_task_start", {{"taskId", "itask_missing"}}).ok,
            "starting an unknown investigation task should fail");
      check(!router.invoke("investigation_task_start", nlohmann::json::object()).ok,
            "investigation_task_start requires a taskId");
      check(!router
                 .invoke("investigation_task_recover",
                         {{"input", {{"taskId", "itask_recover"}, {"unexpected", 1}}}})
                 .ok,
            "investigation_task_recover rejects unexpected input fields");
    }

    // ── Host-owned filesystem backup ledger ──────────────────────────────────
    // The backup bytes stay on the remote server; the host keeps only bounded
    // metadata. A model never supplies the backup path or a byte count the
    // ledger has not already seen, and every mutation must still be bound to
    // the durable task, plan step and exact inputs.
    try {
      const std::string source_path = "/etc/nginx/nginx.conf";
      const std::string second_path = "/etc/nginx/conf.d/site.conf";
      const std::string third_path = "/etc/nginx/conf.d/extra.conf";
      const std::string token(32, 'a');
      const std::string revision(64, 'a');
      const std::string other_revision(64, 'b');
      const auto backup_path = yukinal::desktop_host::backup_path_for(source_path, token);
      const auto second_backup = yukinal::desktop_host::backup_path_for(second_path, token);
      const auto third_backup = yukinal::desktop_host::backup_path_for(third_path, token);

      check(yukinal::desktop_host::is_safe_backup_token(token) &&
                !yukinal::desktop_host::is_safe_backup_token(std::string(32, 'A')) &&
                !yukinal::desktop_host::is_safe_backup_token(std::string(31, 'a')),
            "a backup token must be exactly 32 lowercase hexadecimal characters");
      check(yukinal::desktop_host::is_backup_path_for(source_path, backup_path) &&
                !yukinal::desktop_host::is_backup_path_for(second_path, backup_path) &&
                !yukinal::desktop_host::is_backup_path_for(source_path, second_backup),
            "a derived backup path must belong only to its own source path");
      check(yukinal::desktop_host::backup_record_id("srv_01abc", backup_path) ==
                    yukinal::desktop_host::backup_record_id("srv_01abc", backup_path) &&
                yukinal::desktop_host::backup_record_id("srv_01abc", backup_path) !=
                    yukinal::desktop_host::backup_record_id("srv_other", backup_path) &&
                yukinal::desktop_host::backup_record_id("srv_01abc", backup_path)
                        .rfind("backup_", 0) == 0,
            "a backup record id must be a stable sha256 over the server and backup path");
      bool bad_token_rejected = false;
      try {
        (void)yukinal::desktop_host::backup_path_for(source_path, "xyz");
      } catch (const std::exception&) {
        bad_token_rejected = true;
      }
      check(bad_token_rejected, "an unsafe backup token must be refused");

      const nlohmann::json backup_target = {
          {"host", "remote"}, {"serverId", "srv_01abc"}, {"environment", "staging"}};
      const auto backup_plan = [&](const std::string& plan_id, int revision_number,
                                   const std::string& step_id, const std::string& tool_name,
                                   bool requires_approval, const nlohmann::json& input_bindings) {
        return nlohmann::json{
            {"id", plan_id},
            {"taskId", "itask_backup"},
            {"revision", revision_number},
            {"status", "active"},
            {"createdAt", "2026-01-05T00:00:00Z"},
            {"updatedAt", "2026-01-05T00:00:00Z"},
            {"currentStepId", step_id},
            {"approval", {{"status", "approved"}}},
            {"steps",
             nlohmann::json::array({{{"id", step_id},
                                     {"ordinal", 0},
                                     {"kind", "action"},
                                     {"title", "Mutate the nginx config"},
                                     {"status", "pending"},
                                     {"attempts", 0},
                                     {"maxAttempts", 1},
                                     {"requiresApproval", requires_approval},
                                     {"allowedTools", nlohmann::json::array({tool_name})},
                                     {"evidenceIds", nlohmann::json::array()},
                                     {"inputBindings", input_bindings}}})}};
      };

      // A durable execute-mode task is the prerequisite for every filesystem
      // mutation; the plan step must name the tool and bind the exact inputs.
      const nlohmann::json backup_task = {
          {"id", "itask_backup"},
          {"serverId", "srv_01abc"},
          {"objective", "Back up and restore the nginx config"},
          {"successCriteria", nlohmann::json::array({"nginx.conf restored"})},
          {"scope", backup_target},
          {"guardrails",
           {{"forbiddenTools", nlohmann::json::array()},
            {"forbiddenPathPrefixes", nlohmann::json::array()}}},
          {"mode", "goal"},
          {"permissionMode", "auto"},
          {"automationLevel", "execute"},
          {"phase", "investigating"},
          {"status", "pending"},
          {"budget", {{"maxSteps", 8}, {"maxRunMs", 120000}, {"maxAttempts", 3}}},
          {"createdBy", "user"},
          {"createdAt", "2026-01-05T00:00:00Z"},
          {"updatedAt", "2026-01-05T00:00:00Z"},
      };
      (void)database.create_investigation_task(backup_task);

      const auto register_params = [&](const std::string& path, const std::string& backup,
                                       const std::string& plan_id, const std::string& step_id,
                                       std::int64_t bytes) {
        return nlohmann::json{
            {"serverId", "srv_01abc"},
            {"path", path},
            {"backupPath", backup},
            {"revision", revision},
            {"taskId", "itask_backup"},
            {"planId", plan_id},
            {"planStepId", step_id},
            {"traceId", "trace_backup"},
            {"callId", "call_backup"},
            {"bytesBackedUp", bytes},
            {"target", backup_target},
            {"evidenceIds", nlohmann::json::array()}};
      };

      database.save_investigation_plan(backup_plan(
          "iplan_backup_1", 1, "step_backup_1", "filesystem.backup", false,
          {{"path", source_path}}));
      const auto registered = router.handle_host_request(
          "host.investigation.filesystem_backup.register",
          register_params(source_path, backup_path, "iplan_backup_1", "step_backup_1", 1234));
      check(registered.value("registered", false) == true && registered.size() == 2 &&
                registered.value("backupPath", std::string()) == backup_path,
            "the register RPC should echo the host-owned backup path and nothing else");

      const auto ledger =
          database.find_available_filesystem_backup("srv_01abc", source_path, backup_path);
      check(ledger.has_value() && ledger->at("revision") == revision &&
                ledger->at("bytesBackedUp") == 1234 && ledger->at("status") == "available" &&
                ledger->at("taskId") == "itask_backup" && !ledger->contains("traceId") &&
                !ledger->contains("callId") && !ledger->contains("planId"),
            "the ledger should keep the durable call identity out of the model-visible record");

      bool duplicate_rejected = false;
      try {
        (void)router.handle_host_request(
            "host.investigation.filesystem_backup.register",
            register_params(source_path, backup_path, "iplan_backup_1", "step_backup_1", 1234));
      } catch (const yukinal::desktop_host::SidecarRpcError& error) {
        duplicate_rejected = error.code() == -32000;
      }
      check(duplicate_rejected,
            "registering the same host backup twice should be refused by the ledger");

      bool oversize_rejected = false;
      try {
        (void)router.handle_host_request(
            "host.investigation.filesystem_backup.register",
            register_params(source_path, backup_path, "iplan_backup_1", "step_backup_1",
                            1048577));
      } catch (const yukinal::desktop_host::SidecarRpcError&) {
        oversize_rejected = true;
      }
      check(oversize_rejected, "a backup above the host size limit must be refused");

      bool bad_identity_rejected = false;
      try {
        auto missing_identity = register_params(source_path, backup_path, "iplan_backup_1",
                                                 "step_backup_1", 1);
        missing_identity["traceId"] = "";
        (void)router.handle_host_request("host.investigation.filesystem_backup.register",
                                        missing_identity);
      } catch (const yukinal::desktop_host::SidecarRpcError&) {
        bad_identity_rejected = true;
      }
      check(bad_identity_rejected,
            "a backup registration without a durable call identity must be refused");

      bool wrong_env_rejected = false;
      try {
        auto wrong_env = register_params(source_path, backup_path, "iplan_backup_1",
                                         "step_backup_1", 1);
        wrong_env["target"] = {
            {"host", "remote"}, {"serverId", "srv_01abc"}, {"environment", "production"}};
        (void)router.handle_host_request("host.investigation.filesystem_backup.register",
                                        wrong_env);
      } catch (const yukinal::desktop_host::SidecarRpcError&) {
        wrong_env_rejected = true;
      }
      check(wrong_env_rejected,
            "a backup target whose environment does not match the server must be refused");

      bool unbound_plan_rejected = false;
      try {
        (void)router.handle_host_request(
            "host.investigation.filesystem_backup.register",
            register_params(source_path, backup_path, "iplan_write_2", "step_collect", 1));
      } catch (const yukinal::desktop_host::SidecarRpcError&) {
        unbound_plan_rejected = true;
      }
      check(unbound_plan_rejected,
            "a backup request that is not bound to the active plan must be refused");

      database.save_investigation_plan(backup_plan(
          "iplan_backup_1b", 2, "step_backup_1b", "filesystem.backup", false,
          {{"path", second_path}}));
      const auto second_registered = router.handle_host_request(
          "host.investigation.filesystem_backup.register",
          register_params(second_path, second_backup, "iplan_backup_1b", "step_backup_1b", 2048));
      check(second_registered.value("registered", false) == true &&
                second_registered.value("backupPath", std::string()) == second_backup,
            "a second source path should register its own host-owned backup");

      // A restore must agree with the ledger on the revision and the byte count,
      // and can only consume a backup owned by the same task.
      database.save_investigation_plan(backup_plan(
          "iplan_backup_2", 3, "step_backup_2", "filesystem.restore", false,
          {{"path", source_path},
           {"backupPath", backup_path},
           {"expectedRevision", revision}}));
      const auto restore_params = [&](const std::string& path, const std::string& backup,
                                       const std::string& plan_id, const std::string& step_id,
                                       const std::string& returned_revision,
                                       std::int64_t bytes_after) {
        return nlohmann::json{
            {"serverId", "srv_01abc"},
            {"path", path},
            {"backupPath", backup},
            {"expectedRevision", revision},
            {"revision", returned_revision},
            {"taskId", "itask_backup"},
            {"planId", plan_id},
            {"planStepId", step_id},
            {"traceId", "trace_backup"},
            {"callId", "call_backup"},
            {"bytesBefore", 999},
            {"bytesAfter", bytes_after},
            {"target", backup_target},
            {"evidenceIds", nlohmann::json::array()}};
      };
      bool bytes_mismatch_rejected = false;
      try {
        (void)router.handle_host_request(
            "host.investigation.filesystem_backup.restored",
            restore_params(source_path, backup_path, "iplan_backup_2", "step_backup_2", revision,
                           4321));
      } catch (const yukinal::desktop_host::SidecarRpcError&) {
        bytes_mismatch_rejected = true;
      }
      check(bytes_mismatch_rejected,
            "a restore whose byte count disagrees with the ledger must be refused");
      bool revision_mismatch_rejected = false;
      try {
        (void)router.handle_host_request(
            "host.investigation.filesystem_backup.restored",
            restore_params(source_path, backup_path, "iplan_backup_2", "step_backup_2",
                           other_revision, 1234));
      } catch (const yukinal::desktop_host::SidecarRpcError&) {
        revision_mismatch_rejected = true;
      }
      check(revision_mismatch_rejected,
            "a restore whose revision disagrees with the ledger must be refused");
      bool forged_backup_rejected = false;
      try {
        (void)router.handle_host_request(
            "host.investigation.filesystem_backup.restored",
            restore_params(source_path, "/etc/nginx/not-a-host-backup", "iplan_backup_2",
                           "step_backup_2", revision, 1234));
      } catch (const yukinal::desktop_host::SidecarRpcError&) {
        forged_backup_rejected = true;
      }
      check(forged_backup_rejected,
            "a restore must only consume a host-derived backup path for the same source");

      const auto restored = router.handle_host_request(
          "host.investigation.filesystem_backup.restored",
          restore_params(source_path, backup_path, "iplan_backup_2", "step_backup_2", revision,
                         1234));
      check(restored.value("restored", false) == true &&
                restored.value("path", std::string()) == source_path &&
                restored.value("backupPath", std::string()) == backup_path &&
                restored.value("revision", std::string()) == revision &&
                restored.at("bytesBefore") == 999 && restored.at("bytesAfter") == 1234,
            "the restored RPC should echo the consumed backup and both byte counts");
      check(!database.find_available_filesystem_backup("srv_01abc", source_path, backup_path)
                 .has_value(),
            "a restored backup must leave the available ledger");
      bool consumed_rejected = false;
      try {
        (void)router.handle_host_request(
            "host.investigation.filesystem_backup.restored",
            restore_params(source_path, backup_path, "iplan_backup_2", "step_backup_2", revision,
                           1234));
      } catch (const yukinal::desktop_host::SidecarRpcError&) {
        consumed_rejected = true;
      }
      check(consumed_rejected, "a consumed backup cannot be restored twice");

      const auto restored_list = database.list_filesystem_backups_for_task(
          "srv_01abc", "itask_backup", std::string("restored"), std::nullopt, 10);
      check(restored_list.at("records").size() == 1 &&
                restored_list.at("records").at(0).at("backupPath") == backup_path &&
                restored_list.at("truncated") == false &&
                restored_list.at("records").at(0).contains("restoredAt"),
            "the ledger listing should expose the restored backup and its timestamp");

      // Cleanup consumes an available backup through the exact approved input.
      database.save_investigation_plan(backup_plan(
          "iplan_backup_3", 4, "step_backup_3", "filesystem.backup.cleanup", false,
          {{"path", second_path},
           {"backupPath", second_backup},
           {"expectedRevision", revision}}));
      const auto cleanup_input = nlohmann::json{{"path", second_path},
                                                {"backupPath", second_backup},
                                                {"expectedRevision", revision}};
      const auto deleted_params = [&](const nlohmann::json& input, const nlohmann::json& item,
                                      std::int64_t bytes) {
        return nlohmann::json{
            {"serverId", "srv_01abc"},
            {"revision", revision},
            {"taskId", "itask_backup"},
            {"planId", "iplan_backup_3"},
            {"planStepId", "step_backup_3"},
            {"traceId", "trace_backup"},
            {"callId", "call_backup"},
            {"bytesDeleted", bytes},
            {"input", input},
            {"item", item},
            {"target", backup_target},
            {"evidenceIds", nlohmann::json::array()}};
      };
      bool wrong_item_rejected = false;
      try {
        const auto wrong_item = nlohmann::json{{"path", source_path},
                                               {"backupPath", backup_path},
                                               {"expectedRevision", revision}};
        (void)router.handle_host_request("host.investigation.filesystem_backup.deleted",
                                        deleted_params(cleanup_input, wrong_item, 2048));
      } catch (const yukinal::desktop_host::SidecarRpcError&) {
        wrong_item_rejected = true;
      }
      check(wrong_item_rejected,
            "cleanup must refuse an item that is not part of the approved input");
      bool cleanup_bytes_rejected = false;
      try {
        (void)router.handle_host_request("host.investigation.filesystem_backup.deleted",
                                        deleted_params(cleanup_input, cleanup_input, 999));
      } catch (const yukinal::desktop_host::SidecarRpcError&) {
        cleanup_bytes_rejected = true;
      }
      check(cleanup_bytes_rejected,
            "cleanup whose byte count disagrees with the ledger must be refused");
      const auto deleted = router.handle_host_request(
          "host.investigation.filesystem_backup.deleted",
          deleted_params(cleanup_input, cleanup_input, 2048));
      check(deleted.value("deleted", false) == true &&
                deleted.value("path", std::string()) == second_path &&
                deleted.value("backupPath", std::string()) == second_backup &&
                deleted.value("revision", std::string()) == revision &&
                deleted.at("bytesDeleted") == 2048,
            "the deleted RPC should echo the consumed backup");
      check(!database.find_available_filesystem_backup("srv_01abc", second_path, second_backup)
                 .has_value(),
            "a cleaned backup must leave the available ledger");

      // A backup owned by another task can only be cleaned through an approved
      // batch step; a bare single-item restore is always refused.
      const nlohmann::json other_task = {
          {"id", "itask_backup_other"},
          {"serverId", "srv_01abc"},
          {"objective", "Own a backup for another task"},
          {"successCriteria", nlohmann::json::array({"backup owned"})},
          {"scope", backup_target},
          {"guardrails",
           {{"forbiddenTools", nlohmann::json::array()},
            {"forbiddenPathPrefixes", nlohmann::json::array()}}},
          {"mode", "goal"},
          {"permissionMode", "auto"},
          {"automationLevel", "execute"},
          {"phase", "investigating"},
          {"status", "pending"},
          {"budget", {{"maxSteps", 8}, {"maxRunMs", 120000}, {"maxAttempts", 3}}},
          {"createdBy", "user"},
          {"createdAt", "2026-01-05T00:00:00Z"},
          {"updatedAt", "2026-01-05T00:00:00Z"},
      };
      (void)database.create_investigation_task(other_task);
      auto other_plan = backup_plan("iplan_other_backup", 1, "step_other_backup",
                                    "filesystem.backup", false, {{"path", third_path}});
      other_plan["taskId"] = "itask_backup_other";
      database.save_investigation_plan(other_plan);
      auto other_register = register_params(third_path, third_backup, "iplan_other_backup",
                                            "step_other_backup", 4096);
      other_register["taskId"] = "itask_backup_other";
      const auto third_registered = router.handle_host_request(
          "host.investigation.filesystem_backup.register", other_register);
      check(third_registered.value("registered", false) == true,
            "a second task should be able to register its own backup");

      database.save_investigation_plan(backup_plan(
          "iplan_backup_2b", 5, "step_backup_2b", "filesystem.restore", false,
          {{"path", third_path},
           {"backupPath", third_backup},
           {"expectedRevision", revision}}));
      bool cross_task_restore_rejected = false;
      try {
        (void)router.handle_host_request(
            "host.investigation.filesystem_backup.restored",
            restore_params(third_path, third_backup, "iplan_backup_2b", "step_backup_2b",
                           revision, 4096));
      } catch (const yukinal::desktop_host::SidecarRpcError&) {
        cross_task_restore_rejected = true;
      }
      check(cross_task_restore_rejected,
            "a restore cannot consume a backup owned by another investigation task");

      const nlohmann::json batch_items = nlohmann::json::array(
          {{{"path", third_path},
            {"backupPath", third_backup},
            {"expectedRevision", revision}}});
      database.save_investigation_plan(backup_plan(
          "iplan_backup_batch", 6, "step_backup_batch", "filesystem.backup.cleanup", true,
          {{"items", batch_items.dump()}}));
      const auto batch_input = nlohmann::json{{"items", batch_items}};
      const auto batch_deleted = router.handle_host_request(
          "host.investigation.filesystem_backup.deleted",
          {{"serverId", "srv_01abc"},
           {"revision", revision},
           {"taskId", "itask_backup"},
           {"planId", "iplan_backup_batch"},
           {"planStepId", "step_backup_batch"},
           {"traceId", "trace_backup"},
           {"callId", "call_backup"},
           {"bytesDeleted", 4096},
           {"input", batch_input},
           {"item", batch_items.at(0)},
           {"target", backup_target},
           {"evidenceIds", nlohmann::json::array()}});
      check(batch_deleted.value("deleted", false) == true &&
                batch_deleted.value("backupPath", std::string()) == third_backup,
            "an approved batch step may clean a backup that another task registered");
      check(!database
                 .find_available_filesystem_backup("srv_01abc", third_path, third_backup)
                 .has_value(),
            "the cross-task cleanup must leave the available ledger");

      // The ledger itself is the last line of defence for the record shape.
      const auto backup_record = [&](const std::string& id, const std::string& path,
                                     const std::string& backup, const std::string& record_revision,
                                     const std::string& status, std::int64_t bytes) {
        return nlohmann::json{
            {"id", id},
            {"serverId", "srv_01abc"},
            {"taskId", "itask_backup"},
            {"path", path},
            {"backupPath", backup},
            {"revision", record_revision},
            {"bytesBackedUp", bytes},
            {"status", status},
            {"createdAt", "2026-01-05T00:00:00Z"},
            {"updatedAt", "2026-01-05T00:00:00Z"}};
      };
      bool bad_revision_rejected = false;
      try {
        database.insert_filesystem_backup(backup_record(
            "backup_bad", "/etc/nginx/bad.conf", "/etc/nginx/bad.bak", "not-hex", "available",
            1));
      } catch (const std::invalid_argument&) {
        bad_revision_rejected = true;
      }
      check(bad_revision_rejected, "the ledger should reject a malformed content revision");
      bool bad_status_rejected = false;
      try {
        database.insert_filesystem_backup(backup_record(
            "backup_bad2", "/etc/nginx/bad2.conf", "/etc/nginx/bad2.bak", revision, "consumed",
            1));
      } catch (const std::invalid_argument&) {
        bad_status_rejected = true;
      }
      check(bad_status_rejected, "the ledger should reject an unknown backup status");
      bool control_rejected = false;
      try {
        database.insert_filesystem_backup(backup_record(
            "backup_bad3", std::string("/etc/nginx/bad\n.conf"), "/etc/nginx/bad3.bak",
            revision, "available", 1));
      } catch (const std::invalid_argument&) {
        control_rejected = true;
      }
      check(control_rejected, "the ledger should reject control characters in paths");
      bool negative_bytes_rejected = false;
      try {
        database.insert_filesystem_backup(backup_record(
            "backup_bad4", "/etc/nginx/bad4.conf", "/etc/nginx/bad4.bak", revision, "available",
            -1));
      } catch (const std::invalid_argument&) {
        negative_bytes_rejected = true;
      }
      check(negative_bytes_rejected, "the ledger should reject a negative byte count");
      bool bad_limit_rejected = false;
      try {
        (void)database.list_filesystem_backups_for_task("srv_01abc", "itask_backup", std::nullopt,
                                                        std::nullopt, 0);
      } catch (const std::invalid_argument&) {
        bad_limit_rejected = true;
      }
      check(bad_limit_rejected, "the ledger listing should bound its limit");
      bool bad_status_filter_rejected = false;
      try {
        (void)database.list_filesystem_backups_for_task("srv_01abc", "itask_backup",
                                                        std::string("consumed"), std::nullopt, 10);
      } catch (const std::invalid_argument&) {
        bad_status_filter_rejected = true;
      }
      check(bad_status_filter_rejected, "the ledger listing should reject an unknown status");
      bool bad_scan_limit_rejected = false;
      try {
        (void)database.list_available_filesystem_backups_for_server("srv_01abc", std::nullopt,
                                                                    513);
      } catch (const std::invalid_argument&) {
        bad_scan_limit_rejected = true;
      }
      check(bad_scan_limit_rejected, "the retention scan should bound its limit");
      bool missing_mark_rejected = false;
      try {
        database.mark_filesystem_backup_restored("srv_01abc", "/etc/nginx/missing.conf",
                                                 "/etc/nginx/missing.bak",
                                                 "2026-01-05T00:00:01Z");
      } catch (const std::runtime_error&) {
        missing_mark_rejected = true;
      }
      check(missing_mark_rejected,
            "marking a backup that is not available should report it as unavailable");

      database.insert_filesystem_backup(backup_record("backup_scan_a", "/etc/nginx/scan_a.conf",
                                                      "/etc/nginx/scan_a.bak", revision,
                                                      "available", 64));
      database.insert_filesystem_backup(backup_record("backup_scan_b", "/etc/nginx/scan_b.conf",
                                                      "/etc/nginx/scan_b.bak", revision,
                                                      "available", 64));
      const auto truncated_scan = database.list_available_filesystem_backups_for_server(
          "srv_01abc", std::string("/etc/nginx/"), 1);
      check(truncated_scan.at("records").size() == 1 && truncated_scan.at("truncated") == true,
            "the retention scan should cap its page and flag truncation");
      const auto full_scan = database.list_available_filesystem_backups_for_server(
          "srv_01abc", std::string("/etc/nginx/scan"), 10);
      check(full_scan.at("records").size() == 2 && full_scan.at("truncated") == false,
            "the retention scan should return every available backup under a prefix");
      check(database
                .list_available_filesystem_backups_for_server("srv_01abc",
                                                               std::string("/etc/nginx/scan/"), 10)
                .at("records")
                .empty(),
            "the retention scan should not match a longer path with the same prefix");
    } catch (const std::exception& error) {
      std::cerr << "filesystem backup ledger block failed: " << error.what() << '\n';
      std::exit(3);
    }

    // ── SSH filesystem action targets ────────────────────────────────────────
    // `host.ssh.filesystem_backup_target` derives the host-owned path,
    // `..._restore_target` re-reads the ledger binding, and
    // `..._cleanup_target` reports each item's readiness before the Agent
    // touches the remote server.
    try {
      const std::string bt_path = "/srv/app.conf";
      const std::string bt_revision(64, 'c');
      const nlohmann::json bt_target = {
          {"host", "remote"}, {"serverId", "srv_target"}, {"environment", "staging"}};
      const nlohmann::json bt_task = {
          {"id", "itask_backup_target"},
          {"serverId", "srv_target"},
          {"objective", "Exercise the SSH filesystem action targets"},
          {"successCriteria", nlohmann::json::array({"targets resolved"})},
          {"scope", bt_target},
          {"guardrails",
           {{"forbiddenTools", nlohmann::json::array()},
            {"forbiddenPathPrefixes", nlohmann::json::array()}}},
          {"mode", "goal"},
          {"permissionMode", "auto"},
          {"automationLevel", "execute"},
          {"phase", "investigating"},
          {"status", "pending"},
          {"budget", {{"maxSteps", 8}, {"maxRunMs", 120000}, {"maxAttempts", 3}}},
          {"createdBy", "user"},
          {"createdAt", "2026-01-06T00:00:00Z"},
          {"updatedAt", "2026-01-06T00:00:00Z"},
      };
      (void)database.create_investigation_task(bt_task);

      const auto bt_plan = [&](const std::string& plan_id, int revision_number,
                               const std::string& step_id, const std::string& tool_name,
                               const nlohmann::json& input_bindings) {
        return nlohmann::json{
            {"id", plan_id},
            {"taskId", "itask_backup_target"},
            {"revision", revision_number},
            {"status", "active"},
            {"createdAt", "2026-01-06T00:00:00Z"},
            {"updatedAt", "2026-01-06T00:00:00Z"},
            {"currentStepId", step_id},
            {"approval", {{"status", "approved"}}},
            {"steps",
             nlohmann::json::array({{{"id", step_id},
                                     {"ordinal", 0},
                                     {"kind", "action"},
                                     {"title", "Mutate the remote config"},
                                     {"status", "pending"},
                                     {"attempts", 0},
                                     {"maxAttempts", 1},
                                     {"allowedTools", nlohmann::json::array({tool_name})},
                                     {"evidenceIds", nlohmann::json::array()},
                                     {"inputBindings", input_bindings}}})}};
      };
      yukinal::desktop_host::IpcRouter target_router(&database, &target_credentials);

      database.save_investigation_plan(
          bt_plan("iplan_bt_1", 1, "step_bt_backup", "filesystem.backup", {{"path", bt_path}}));
      const auto backup_target_payload = target_router.handle_host_request(
          "host.ssh.filesystem_backup_target",
          {{"serverId", "srv_target"},
           {"path", bt_path},
           {"taskId", "itask_backup_target"},
           {"planId", "iplan_bt_1"},
           {"planStepId", "step_bt_backup"},
           {"evidenceIds", nlohmann::json::array()},
           {"target", bt_target}});
      const auto derived_backup =
          backup_target_payload.at("filesystemBackup").at("backupPath").get<std::string>();
      check(backup_target_payload.at("target").at("target_id") == "srv_target" &&
                backup_target_payload.at("credentials").at("YK_SSH_PASSWORD") == "s3cret-pass" &&
                backup_target_payload.at("filesystemBackup").at("path") == bt_path &&
                yukinal::desktop_host::is_backup_path_for(bt_path, derived_backup),
            "the backup target should derive a host-owned path and resolve the SSH service");

      bool unbound_backup_rejected = false;
      try {
        (void)target_router.handle_host_request(
            "host.ssh.filesystem_backup_target",
            {{"serverId", "srv_target"},
             {"path", bt_path},
             {"taskId", "itask_backup_target"},
             {"planId", "iplan_bt_1"},
             {"planStepId", "step_bt_backup"},
             {"evidenceIds", nlohmann::json::array()},
             {"target", {{"host", "remote"}, {"serverId", "srv_target"}, {"environment", "production"}}}});
      } catch (const yukinal::desktop_host::SidecarRpcError&) {
        unbound_backup_rejected = true;
      }
      check(unbound_backup_rejected,
            "the backup target should refuse a target that does not match the server");

      const auto bt_registered = target_router.handle_host_request(
          "host.investigation.filesystem_backup.register",
          {{"serverId", "srv_target"},
           {"path", bt_path},
           {"backupPath", derived_backup},
           {"revision", bt_revision},
           {"bytesBackedUp", 2048},
           {"taskId", "itask_backup_target"},
           {"planId", "iplan_bt_1"},
           {"planStepId", "step_bt_backup"},
           {"traceId", "trace_bt"},
           {"callId", "call_bt"},
           {"evidenceIds", nlohmann::json::array()},
           {"target", bt_target}});
      check(bt_registered.value("registered", false) == true,
            "the derived backup should register in the ledger");

      database.save_investigation_plan(bt_plan(
          "iplan_bt_2", 2, "step_bt_restore", "filesystem.restore",
          {{"path", bt_path},
           {"backupPath", derived_backup},
           {"expectedRevision", bt_revision}}));
      const auto restore_target_payload = target_router.handle_host_request(
          "host.ssh.filesystem_restore_target",
          {{"serverId", "srv_target"},
           {"path", bt_path},
           {"backupPath", derived_backup},
           {"expectedRevision", bt_revision},
           {"taskId", "itask_backup_target"},
           {"planId", "iplan_bt_2"},
           {"planStepId", "step_bt_restore"},
           {"evidenceIds", nlohmann::json::array()},
           {"target", bt_target}});
      check(restore_target_payload.at("filesystemRestore").at("backupPath") == derived_backup &&
                restore_target_payload.at("filesystemRestore").at("revision") == bt_revision &&
                restore_target_payload.at("filesystemRestore").at("bytesBackedUp") == 2048 &&
                restore_target_payload.at("target").at("target_id") == "srv_target",
            "the restore target should carry the ledger revision and byte count");
      bool forged_restore_rejected = false;
      try {
        (void)target_router.handle_host_request(
            "host.ssh.filesystem_restore_target",
            {{"serverId", "srv_target"},
             {"path", bt_path},
             {"backupPath", "/srv/not-a-host-backup"},
             {"expectedRevision", bt_revision},
             {"taskId", "itask_backup_target"},
             {"planId", "iplan_bt_2"},
             {"planStepId", "step_bt_restore"},
             {"evidenceIds", nlohmann::json::array()},
             {"target", bt_target}});
      } catch (const yukinal::desktop_host::SidecarRpcError&) {
        forged_restore_rejected = true;
      }
      check(forged_restore_rejected,
            "the restore target should refuse a backup path the host did not derive");

      database.save_investigation_plan(bt_plan(
          "iplan_bt_3", 3, "step_bt_cleanup", "filesystem.backup.cleanup",
          {{"path", bt_path},
           {"backupPath", derived_backup},
           {"expectedRevision", bt_revision}}));
      const auto cleanup_target_payload = target_router.handle_host_request(
          "host.ssh.filesystem_cleanup_target",
          {{"serverId", "srv_target"},
           {"input",
            {{"path", bt_path},
             {"backupPath", derived_backup},
             {"expectedRevision", bt_revision}}},
           {"taskId", "itask_backup_target"},
           {"planId", "iplan_bt_3"},
           {"planStepId", "step_bt_cleanup"},
           {"runId", "run_bt"},
           {"traceId", "trace_bt"},
           {"callId", "call_bt"},
           {"evidenceIds", nlohmann::json::array()},
           {"target", bt_target}});
      const auto& cleanup_items =
          cleanup_target_payload.at("filesystemCleanup").at("items");
      check(cleanup_items.size() == 1 && cleanup_items.at(0).at("status") == "ready" &&
                cleanup_items.at(0).at("revision") == bt_revision &&
                cleanup_items.at(0).at("bytesBackedUp") == 2048 &&
                cleanup_items.at(0).at("ownerTaskId") == "itask_backup_target" &&
                cleanup_target_payload.at("target").at("target_id") == "srv_target",
            "the cleanup target should report a ready host-owned item and resolve the service");

      const std::string unknown_path = "/srv/other.conf";
      const auto unknown_backup = yukinal::desktop_host::backup_path_for(
          unknown_path, std::string(32, 'd'));
      const nlohmann::json bt_batch_items = nlohmann::json::array(
          {{{"path", unknown_path},
            {"backupPath", unknown_backup},
            {"expectedRevision", bt_revision}},
           {{"path", bt_path},
            {"backupPath", derived_backup},
            {"expectedRevision", bt_revision}}});
      database.save_investigation_plan(bt_plan(
          "iplan_bt_4", 4, "step_bt_cleanup_batch", "filesystem.backup.cleanup",
          {{"items", bt_batch_items.dump()}}));
      const auto batch_cleanup_payload = target_router.handle_host_request(
          "host.ssh.filesystem_cleanup_target",
          {{"serverId", "srv_target"},
           {"input", {{"items", bt_batch_items}}},
           {"taskId", "itask_backup_target"},
           {"planId", "iplan_bt_4"},
           {"planStepId", "step_bt_cleanup_batch"},
           {"runId", "run_bt"},
           {"traceId", "trace_bt"},
           {"callId", "call_bt"},
           {"evidenceIds", nlohmann::json::array()},
           {"target", bt_target}});
      const auto& batch_items = batch_cleanup_payload.at("filesystemCleanup").at("items");
      check(batch_items.size() == 2 && batch_items.at(0).at("status") == "skipped" &&
                batch_items.at(0).at("reason") ==
                    "no available host ledger record for this server and backup path" &&
                batch_items.at(1).at("status") == "ready" &&
                batch_cleanup_payload.contains("target"),
            "the batch cleanup target should skip an unknown item and keep the ready one");

      database.save_investigation_plan(bt_plan(
          "iplan_bt_5", 5, "step_bt_cleanup_missing", "filesystem.backup.cleanup",
          {{"path", unknown_path},
           {"backupPath", unknown_backup},
           {"expectedRevision", bt_revision}}));
      bool missing_cleanup_rejected = false;
      try {
        (void)target_router.handle_host_request(
            "host.ssh.filesystem_cleanup_target",
            {{"serverId", "srv_target"},
             {"input",
              {{"path", unknown_path},
               {"backupPath", unknown_backup},
               {"expectedRevision", bt_revision}}},
             {"taskId", "itask_backup_target"},
             {"planId", "iplan_bt_5"},
             {"planStepId", "step_bt_cleanup_missing"},
             {"runId", "run_bt"},
             {"traceId", "trace_bt"},
             {"callId", "call_bt"},
             {"evidenceIds", nlohmann::json::array()},
             {"target", bt_target}});
      } catch (const yukinal::desktop_host::SidecarRpcError&) {
        missing_cleanup_rejected = true;
      }
      check(missing_cleanup_rejected,
            "a single cleanup target for an unregistered backup must be refused");

      bool bad_identity_rejected = false;
      try {
        (void)target_router.handle_host_request(
            "host.ssh.filesystem_cleanup_target",
            {{"serverId", "srv_target"},
             {"input",
              {{"path", bt_path},
               {"backupPath", derived_backup},
               {"expectedRevision", bt_revision}}},
             {"taskId", "itask_backup_target"},
             {"planId", "iplan_bt_3"},
             {"planStepId", "step_bt_cleanup"},
             {"runId", ""},
             {"traceId", "trace_bt"},
             {"callId", "call_bt"},
             {"evidenceIds", nlohmann::json::array()},
             {"target", bt_target}});
      } catch (const yukinal::desktop_host::SidecarRpcError&) {
        bad_identity_rejected = true;
      }
      check(bad_identity_rejected,
            "the cleanup target should require a durable call identity");
    } catch (const std::exception& error) {
      std::cerr << "SSH filesystem action target block failed: " << error.what() << '\n';
      std::exit(4);
    }

    // ── Host tool call idempotency ledger ────────────────────────────────────
    try {
      const std::string running_fingerprint(64, 'a');
      const std::string running_action(64, 'b');
      const nlohmann::json ledger_claim = {
          {"traceId", "trace_ledger"},
          {"callId", "call_ledger"},
          {"taskId", "itask_backup_target"},
          {"planId", "iplan_ledger"},
          {"planStepId", "step_ledger"},
          {"toolName", "filesystem.write"},
          {"requestFingerprint", running_fingerprint},
          {"actionFingerprint", running_action},
          {"startedAt", "2026-01-06T00:00:00Z"}};
      check(database.claim_host_tool_call(ledger_claim).value("claimed", false) == true,
            "a new guarded host tool call should be claimed");
      check(database.host_tool_call_status("trace_ledger", "call_ledger").value_or("") == "running",
            "a claimed host tool call should be recorded as running");
      const auto duplicate_claim = database.claim_host_tool_call(ledger_claim);
      check(duplicate_claim.value("claimed", true) == false &&
                duplicate_claim.value("requestFingerprint", std::string()) == running_fingerprint &&
                duplicate_claim.value("status", std::string()) == "running",
            "claiming the same host tool call key should return the existing row");
      nlohmann::json resumed_claim = ledger_claim;
      resumed_claim["callId"] = "call_ledger_resumed";
      check(database.claim_host_tool_call(resumed_claim).value("claimed", true) == false,
            "a new call id for the same running planned action should be blocked");
      database.finish_host_tool_call("trace_ledger", "call_ledger", "success",
                                     "2026-01-06T00:01:00Z");
      check(database.host_tool_call_status("trace_ledger", "call_ledger").value_or("") == "success",
            "finishing a host tool call should record its terminal status");
      bool double_finish_rejected = false;
      try {
        database.finish_host_tool_call("trace_ledger", "call_ledger", "failed",
                                       "2026-01-06T00:02:00Z");
      } catch (const std::invalid_argument&) {
        double_finish_rejected = true;
      }
      check(double_finish_rejected, "a host tool call may only be finished once");
      bool bad_status_rejected = false;
      try {
        database.finish_host_tool_call("trace_ledger", "call_ledger", "bogus",
                                       "2026-01-06T00:03:00Z");
      } catch (const std::invalid_argument&) {
        bad_status_rejected = true;
      }
      check(bad_status_rejected, "an unknown host tool call status must be refused");
    } catch (const std::exception& error) {
      std::cerr << "host tool call ledger block failed: " << error.what() << '\n';
      std::exit(5);
    }

    // ── Filesystem backup metadata queries ───────────────────────────────────
    // `host.filesystem.backup.list_query` and `..._retention_scan` are guarded
    // reads: they resolve the durable plan binding, then serve the host-owned
    // ledger for the task's registered server only.
    try {
      yukinal::desktop_host::IpcRouter query_router(&database, &target_credentials);
      const std::string query_path = "/srv/app.conf";
      const std::string query_unknown = "/srv/other.conf";
      const nlohmann::json query_target = {
          {"host", "remote"}, {"serverId", "srv_target"}, {"environment", "staging"}};
      const auto query_plan = [&](const std::string& plan_id, int revision_number,
                                  const std::string& step_id, const std::string& tool_name,
                                  const nlohmann::json& input_bindings) {
        return nlohmann::json{
            {"id", plan_id},
            {"taskId", "itask_backup_target"},
            {"revision", revision_number},
            {"status", "active"},
            {"createdAt", "2026-01-06T00:00:00Z"},
            {"updatedAt", "2026-01-06T00:00:00Z"},
            {"currentStepId", step_id},
            {"approval", {{"status", "approved"}}},
            {"steps",
             nlohmann::json::array({{{"id", step_id},
                                     {"ordinal", 0},
                                     {"kind", "action"},
                                     {"title", "Query the backup metadata"},
                                     {"status", "pending"},
                                     {"attempts", 0},
                                     {"maxAttempts", 1},
                                     {"allowedTools", nlohmann::json::array({tool_name})},
                                     {"evidenceIds", nlohmann::json::array()},
                                     {"inputBindings", input_bindings}}})}};
      };
      const auto query_backups =
          database.list_available_filesystem_backups_for_server("srv_target", std::nullopt, 10);
      check(query_backups.at("records").size() == 1,
            "the query fixture should see exactly one available server backup");
      const auto query_backup =
          query_backups.at("records").at(0).at("backupPath").get<std::string>();

      database.save_investigation_plan(query_plan("iplan_bt_6", 6, "step_bt_list",
                                                  "filesystem.backup.list", {{"path", query_path}}));
      const nlohmann::json list_call = {
          {"callId", "call_list_1"},
          {"traceId", "trace_list_1"},
          {"toolName", "filesystem.backup.list"},
          {"input", {{"path", query_path}}},
          {"target", query_target},
          {"taskId", "itask_backup_target"},
          {"planId", "iplan_bt_6"},
          {"planStepId", "step_bt_list"},
          {"evidenceIds", nlohmann::json::array()}};
      const auto list = query_router.handle_host_request("host.filesystem.backup.list_query",
                                                         {{"toolCall", list_call}});
      check(list.value("status", std::string()) == "success" &&
                list.at("output").at("truncated") == false &&
                list.at("output").at("records").size() == 1 &&
                list.at("output").at("records").at(0).at("backupPath") == query_backup &&
                list.at("output").at("records").at(0).at("status") == "available",
            "the backup list query should return the host-owned ledger record");
      const auto list_replay = query_router.handle_host_request(
          "host.filesystem.backup.list_query", {{"toolCall", list_call}});
      check(list_replay == list,
            "an identical guarded host call should replay its in-memory response");
      check(database.host_tool_call_status("trace_list_1", "call_list_1").value_or("") == "success",
            "a completed guarded query should close its ledger row");

      // The same plan step with a tampered input fails the plan binding.
      nlohmann::json tampered_call = list_call;
      tampered_call["input"] = {{"path", query_unknown}};
      const auto tampered = query_router.handle_host_request("host.filesystem.backup.list_query",
                                                             {{"toolCall", tampered_call}});
      check(tampered.value("status", std::string()) == "failed" &&
                tampered.at("error").at("code") == "plan_deviation",
            "a guarded query whose input deviates from its plan step must be refused");

      // A reused call id bound to a different host action is a hard refusal.
      database.save_investigation_plan(query_plan("iplan_bt_7", 7, "step_bt_list2",
                                                  "filesystem.backup.list",
                                                  {{"path", query_unknown}}));
      nlohmann::json reused_call = list_call;
      reused_call["input"] = {{"path", query_unknown}};
      reused_call["planId"] = "iplan_bt_7";
      reused_call["planStepId"] = "step_bt_list2";
      const auto reused = query_router.handle_host_request("host.filesystem.backup.list_query",
                                                           {{"toolCall", reused_call}});
      check(reused.value("status", std::string()) == "failed" &&
                reused.at("error").at("code") == "plan_deviation" &&
                reused.at("error").at("detail").at("code") == "duplicate_call" &&
                reused.at("error").at("detail").at("action") == "deny",
            "reusing a call id for a different host action must be refused");

      database.save_investigation_plan(query_plan("iplan_bt_8", 8, "step_bt_retention",
                                                  "filesystem.backup.retention",
                                                  {{"pathPrefix", "/srv"}}));
      const nlohmann::json retention_call = {
          {"callId", "call_ret_1"},
          {"traceId", "trace_ret_1"},
          {"toolName", "filesystem.backup.retention"},
          {"input", {{"pathPrefix", "/srv"}, {"keepLatest", 1}}},
          {"target", query_target},
          {"taskId", "itask_backup_target"},
          {"planId", "iplan_bt_8"},
          {"planStepId", "step_bt_retention"},
          {"evidenceIds", nlohmann::json::array()}};
      const auto retention = query_router.handle_host_request(
          "host.filesystem.backup.retention_scan", {{"toolCall", retention_call}});
      check(retention.value("status", std::string()) == "success" &&
                retention.at("output").at("truncated") == false &&
                retention.at("output").at("records").size() == 1 &&
                retention.at("output").at("records").at(0).at("path") == query_path &&
                retention.at("output").at("records").at(0).at("bytesBackedUp") == 2048 &&
                retention.at("output").at("evaluatedAtEpochSeconds").is_number_integer(),
            "the retention scan should return the available server backups with a scan clock");

      const auto missing_filter = query_router.handle_host_request(
          "host.filesystem.backup.retention_scan",
          {{"toolCall",
            {{"callId", "call_ret_bad"},
             {"traceId", "trace_ret_bad"},
             {"toolName", "filesystem.backup.retention"},
             {"input", nlohmann::json::object()},
             {"target", query_target},
             {"taskId", "itask_backup_target"}}}});
      check(missing_filter.value("status", std::string()) == "failed" &&
                missing_filter.at("error").at("code") == "invalid_input",
            "the retention scan should require keepLatest or olderThanDays");

      bool bad_envelope_rejected = false;
      try {
        (void)query_router.handle_host_request("host.filesystem.backup.list_query",
                                               {{"toolCall", nlohmann::json::object()}});
      } catch (const yukinal::desktop_host::SidecarRpcError& error) {
        bad_envelope_rejected = error.code() == -32602;
      }
      check(bad_envelope_rejected,
            "a malformed backup query envelope must be a parameter error");
    } catch (const std::exception& error) {
      std::cerr << "filesystem backup query block failed: " << error.what() << '\n';
      std::exit(6);
    }
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

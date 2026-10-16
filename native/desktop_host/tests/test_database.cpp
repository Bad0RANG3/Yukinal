#include "yukinal/desktop_host/database.hpp"
#include "yukinal/desktop_host/desktop_mcp.hpp"
#include "yukinal/desktop_host/desktop_target.hpp"
#include "yukinal/desktop_host/host_trust.hpp"
#include "yukinal/desktop_host/ipc_router.hpp"
#include "schema_migrations.hpp"

#include <chrono>
#include <cstddef>
#include <cstdint>
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

}  // namespace

int main(int argc, char** argv) {
  using yukinal::desktop_host::DesktopDatabase;
  if (argc >= 2 && std::string(argv[1]) == "--mcp-mock") return run_mcp_mock();
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

#include "yukinal/desktop_host/database.hpp"
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

    database.create_chat_session("chat_global", std::nullopt, std::nullopt, "Global", "2026-10-17T01:00:00Z");
    database.create_chat_session("chat_server", std::nullopt, std::string("srv_01abc"), "Server", "2026-10-17T01:00:00Z");
    const nlohmann::json participant = {{"id", "blue-logs"}, {"label", "Blue"}, {"kind", "agent"}};
    const auto appended = router.invoke("chat_message_append", {
      {"sessionId", "chat_server"}, {"role", "assistant"}, {"content", "Evidence"},
      {"participant", participant}, {"model", "test-model"}});
    check(appended.ok && appended.value.at("message").at("participant") == participant,
          "assistant actor metadata should be returned");
    const auto reopened = database.get_chat_session("chat_server");
    check(reopened.at("messages").at(0).at("model") == "test-model" &&
          reopened.at("messages").at(0).at("participant") == participant,
          "assistant model and actor must survive reopening");
    check(router.invoke("chat_session_list", {{"serverId", nullptr}}).value.at("sessions").size() == 1,
          "explicit global history must exclude server conversations");
    const auto scoped_history = router.invoke("chat_session_list", {{"serverId", "srv_01abc"}});
    check(scoped_history.ok && scoped_history.value.at("sessions").size() == 1 &&
          scoped_history.value.at("counts").at("active") == 1,
          "history and counts must use the same server scope");
    check(!router.invoke("chat_message_append", {{"sessionId", "chat_server"}, {"role", "assistant"},
          {"content", "Bad"}, {"participant", {{"id", "x"}, {"label", "x"}, {"kind", "root"}}}}).ok,
          "invalid actors must be rejected at the host boundary");

    (void)database.create_chat_session("chat_order", std::nullopt, std::nullopt,
        "Append order", "2026-10-17T01:00:00Z");
    for (const auto* id : {"z_prior", "m_current", "a_reply"}) {
      (void)database.append_chat_message(id, "chat_order", "user", id,
          std::nullopt, std::nullopt, "2026-10-17T01:00:01Z");
    }
    // A display clock adjustment must not move a later append ahead of a turn.
    (void)database.append_chat_message("0_clock_adjusted", "chat_order", "assistant",
        "Latest append", std::nullopt, std::nullopt, "2026-10-17T00:59:59Z");
    const auto ordered = database.get_chat_session("chat_order");
    check(ordered.at("messages").at(0).at("id") == "z_prior" &&
          ordered.at("messages").at(1).at("id") == "m_current" &&
          ordered.at("messages").at(2).at("id") == "a_reply" &&
          ordered.at("messages").at(3).at("id") == "0_clock_adjusted",
          "chat order must follow appends despite same-second IDs or clock changes");
    check(ordered.at("session").at("lastMessagePreview") == "Latest append",
          "the latest message preview must use the same append order as history");

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

    const auto deleted_mcp =
        mcp_router.invoke("mcp_server_delete", {{"serverId", "ops-tools"}});
    check(deleted_mcp.ok && deleted_mcp.value.at("deleted") == true &&
              deleted_mcp.value.at("stopped") == false,
          "mcp_server_delete should remove the row and report nothing running");
    check(mcp_credentials.size() == 0,
          "deleting an MCP server should reclaim its header secret");
    check(!mcp_router.invoke("mcp_server_delete", {{"serverId", "ops-tools"}}).ok,
          "deleting an unknown MCP server should fail");
  }

  sqlite3* readback = nullptr;
  const auto file = (temporary.path / "yukinal.db").string();
  check(sqlite3_open_v2(file.c_str(), &readback, SQLITE_OPEN_READONLY, nullptr) == SQLITE_OK,
        "could not reopen the temporary database read-only");
  check(query_integer(readback, "PRAGMA user_version") == 37,
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
  check(query_integer(fresh, "PRAGMA user_version") == 37,
        "a new database should be migrated to the supported schema version");
  sqlite3_close(fresh);
  return 0;
}

#include "yukinal/desktop_host/credential_store.hpp"
#include "yukinal/desktop_host/database.hpp"
#include "yukinal/desktop_host/ipc_router.hpp"
#include "yukinal/desktop_host/sidecar.hpp"

#include <atomic>
#include <chrono>
#include <csignal>
#include <cstddef>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <map>
#include <memory>
#include <string>
#include <string_view>
#include <thread>
#include <vector>

#ifdef _WIN32
#include <windows.h>
#endif

namespace {

void check(bool condition, const char* message) {
  if (condition) return;
  std::cerr << message << '\n';
  std::exit(1);
}

// A hermetic credential store so the host RPC boundary can be exercised without
// writing to the operating system credential store.
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

struct TemporaryDirectory {
  std::filesystem::path path =
      std::filesystem::temp_directory_path() /
      ("yukinal-sidecar-test-" +
       std::to_string(std::chrono::steady_clock::now().time_since_epoch().count()));

  TemporaryDirectory() { std::filesystem::create_directories(path); }
  ~TemporaryDirectory() {
    std::error_code error;
    std::filesystem::remove_all(path, error);
  }
};

}  // namespace

int run_tests(int argc, char** argv) {
  using nlohmann::json;
  using yukinal::desktop_host::CredentialReference;
  using yukinal::desktop_host::DesktopDatabase;
  using yukinal::desktop_host::Secret;
  using yukinal::desktop_host::SidecarClient;
  using yukinal::desktop_host::SidecarLaunch;
  using yukinal::desktop_host::SidecarRpcError;

  if (argc < 3) {
    std::cerr << "usage: yukinal_desktop_host_sidecar_tests <python> <agent-src>\n";
    return 2;
  }

  TemporaryDirectory directory;
  MemoryCredentialStore credentials;
  DesktopDatabase database(directory.path);

  // A task conversation is initialized once, preserving both scope and history.
  const auto original_chat = database.ensure_chat_session(
      "ses_ensure", std::nullopt, std::string("srv_fixture"),
      "Original title", "2026-01-01T00:00:00Z");
  (void)database.append_chat_message("msg_ensure", "ses_ensure", "user", "Retain this context",
                                     std::nullopt, std::nullopt, "2026-01-01T00:00:01Z");
  const auto reused_chat = database.ensure_chat_session(
      "ses_ensure", std::nullopt, std::string("srv_fixture"),
      "Must not replace title", "2026-01-02T00:00:00Z");
  check(reused_chat.at("messages").size() == 1 &&
            reused_chat.at("messages").at(0).at("content") == "Retain this context" &&
            reused_chat.at("session").at("title") == original_chat.at("session").at("title") &&
            reused_chat.at("session").at("createdAt") == original_chat.at("session").at("createdAt"),
        "ensuring an existing conversation must preserve its transcript and metadata");
  for (bool change_workspace : {false, true}) {
    bool scope_rejected = false;
    try {
      (void)database.ensure_chat_session(
          "ses_ensure", change_workspace ? std::optional<std::string>("workspace_other") : std::nullopt,
          std::string(change_workspace ? "srv_fixture" : "srv_other"), "Other", "2026-01-03T00:00:00Z");
    } catch (const std::invalid_argument&) {
      scope_rejected = true;
    }
    check(scope_rejected && database.get_chat_session("ses_ensure") == reused_chat,
          "a different server or workspace must be rejected without modifying saved history");
  }
  {
    TemporaryDirectory broken_directory;
    DesktopDatabase broken(broken_directory.path);
    sqlite3* connection = nullptr;
    const auto path = (broken_directory.path / "yukinal.db").string();
    check(sqlite3_open(path.c_str(), &connection) == SQLITE_OK,
          "the private error fixture must open");
    check(sqlite3_exec(connection, "DROP TABLE chat_messages", nullptr, nullptr, nullptr) == SQLITE_OK,
          "the private error fixture must expose a real SQL read failure");
    bool read_failed = false;
    try {
      (void)broken.ensure_chat_session("ses_sql_failure", std::nullopt, std::nullopt,
                                       "Must fail", "2026-01-01T00:00:00Z");
    } catch (const std::runtime_error&) {
      read_failed = true;
    }
    sqlite3_stmt* count = nullptr;
    check(sqlite3_prepare_v2(connection,
              "SELECT count(*) FROM chat_sessions WHERE id = 'ses_sql_failure'", -1, &count, nullptr) == SQLITE_OK &&
              sqlite3_step(count) == SQLITE_ROW && sqlite3_column_int(count, 0) == 0,
          "a failed context read must roll back its new conversation");
    sqlite3_finalize(count);
    sqlite3_close(connection);
    check(read_failed, "a real database read failure must propagate rather than become empty history");
  }

  auto handle_host = [&credentials](const std::string& method, const json& params,
                                    const std::shared_ptr<std::atomic<bool>>& cancel) -> json {
    (void)cancel;
    if (method == "host.mcp.catalog") {
      return {{"servers", json::array()}, {"failures", json::array()}};
    }
    if (method == "host.credentials.set") {
      if (!params.contains("service") || !params.contains("account") ||
          !params.contains("secret")) {
        throw yukinal::desktop_host::SidecarRpcError(-32602, "invalid credentials.set params");
      }
      const auto reference = credentials.set(
          params.at("service").get<std::string>(),
          params.at("account").get<std::string>(),
          Secret{std::string_view(params.at("secret").get<std::string>())});
      return {{"credentialRef", reference.to_string()}};
    }
    if (method == "host.credentials.get") {
      const auto reference =
          CredentialReference::parse(params.at("reference").get<std::string>());
      const auto secret = credentials.get(reference);
      return {{"secret", std::string(reinterpret_cast<const char*>(secret.data()),
                                     secret.size())}};
    }
    if (method == "host.credentials.reclaim") {
      credentials.erase(CredentialReference::parse(params.at("reference").get<std::string>()));
      return {{"reclaimed", true}};
    }
    if (method == "host.tool.cancel") {
      return {{"cancelled", false}};
    }
    throw yukinal::desktop_host::SidecarRpcError(-32601,
                                                 "unhandled host method " + method);
  };

  SidecarClient client;
  SidecarLaunch launch;
  launch.program = argv[1];
  launch.args = {"-m", "yukinal_agent.sidecar"};
  launch.environment.push_back({"PYTHONPATH", argv[2]});
  launch.data_directory = directory.path.string();
  launch.client_version = "sidecar-test";
  client.configure(launch);
  client.set_host_handler(handle_host);

  // REGRESSION TEST: a failed spawn must not publish a running state. Before the
  // fix, start() set process_started before spawn_process(), and the following
  // stop() waited forever on a monitor thread that had never been created.
  {
    SidecarClient broken;
    SidecarLaunch broken_launch;
    broken_launch.program = directory.path / "definitely-not-a-sidecar-program";
    broken.configure(broken_launch);
    bool spawn_rejected = false;
    try {
      (void)broken.start(std::chrono::milliseconds(1000));
    } catch (const yukinal::desktop_host::SidecarError&) {
      spawn_rejected = true;
    }
    check(spawn_rejected, "start() with a nonexistent sidecar program should throw");
    check(!broken.status().running, "a failed sidecar spawn must not report running");
    const auto stop_started = std::chrono::steady_clock::now();
    broken.stop(std::chrono::milliseconds(1000));
    const auto stop_elapsed = std::chrono::steady_clock::now() - stop_started;
    check(stop_elapsed < std::chrono::seconds(5),
          "stop() after a failed sidecar spawn must return within five seconds");
    check(!broken.status().running, "stop() after a failed spawn must report stopped");
  }

  const auto started = client.start(std::chrono::milliseconds(60000));
  check(started.pid != 0, "the sidecar should report a process id");
  check(started.tool_count > 0, "the sidecar should expose at least one tool");
  check(!started.agent_version.empty(), "the sidecar should report its agent version");

  const auto ping = client.request("system.ping", {{"echo", "yukinal"}});
  check(ping.value("pong", "") == "yukinal", "the sidecar should answer a ping");

  const auto saved = client.request("provider.save",
                                    {{"kind", "openai-compatible"},
                                     {"label", "Sidecar test"},
                                     {"baseUrl", "http://127.0.0.1:1/v1"},
                                     {"model", "test-model"},
                                     {"apiKey", "sk-sidecar-test"}});
  check(saved.contains("provider") && saved.at("provider").is_object(),
        "provider.save should return the saved provider");
  const auto provider_id = saved.at("provider").value("id", std::string());
  check(!provider_id.empty(), "provider.save should return an id");
  check(saved.at("provider").contains("apiKeyCredentialRef"),
        "a saved provider with an API key should reference the credential store");
  check(credentials.size() == 1, "the API key should live only in the host credential store");

  const auto listed = client.request("provider.list", json::object());
  check(listed.contains("providers") && listed.at("providers").is_array() &&
            listed.at("providers").size() == 1,
        "provider.list should return the saved provider");
  check(listed.at("providers").at(0).value("enabled", false),
        "the saved provider should be active");

  const auto activated = client.request("provider.activate", {{"providerId", provider_id}});
  check(activated.contains("provider"), "provider.activate should return the provider");

  const auto deleted = client.request("provider.delete", {{"providerId", provider_id}});
  check(deleted.value("deleted", false), "provider.delete should delete the provider");
  check(deleted.value("credentialReclaimed", false),
        "provider.delete should reclaim the unshared credential");
  check(credentials.size() == 0, "the credential should be reclaimed from the host store");

  client.stop();
  check(!client.status().running, "the sidecar should stop cleanly");

  bool reported_not_running = false;
  try {
    (void)client.request("system.ping", {{"echo", "after"}});
  } catch (const std::exception&) {
    reported_not_running = true;
  }
  check(reported_not_running, "requests after stop should fail");

  // End-to-end through the IPC router: an env-configured sidecar, the host
  // credential RPC, and the provider command forwarding.
#ifdef _WIN32
  SetEnvironmentVariableA("YUKINAL_AGENT_PYTHON", argv[1]);
  SetEnvironmentVariableA("YUKINAL_AGENT_SOURCE", argv[2]);
#else
  setenv("YUKINAL_AGENT_PYTHON", argv[1], 1);
  setenv("YUKINAL_AGENT_SOURCE", argv[2], 1);
#endif
  yukinal::desktop_host::IpcRouter router(&database, &credentials);
  const auto idle = router.invoke("agent_status", json::object());
  check(idle.ok && idle.value.value("running", true) == false,
        "agent_status should report an idle sidecar");

  const auto spawned = router.invoke("agent_spawn", json::object());
  check(spawned.ok && spawned.value.value("toolCount", 0) > 0,
        "agent_spawn should start the sidecar through the router");
  const auto running = router.invoke("agent_status", json::object());
  check(running.ok && running.value.value("running", false),
        "agent_status should report a running sidecar");

  const auto router_saved = router.invoke(
      "provider_save", {{"kind", "openai-compatible"}, {"label", "Router test"},
                        {"baseUrl", "http://127.0.0.1:1/v1"}, {"model", "router-model"},
                        {"apiKey", "sk-router-test"}});
  check(router_saved.ok && router_saved.value.contains("provider"),
        "provider_save should succeed through the router");
  const auto router_id = router_saved.value.at("provider").value("id", std::string());
  check(credentials.size() == 1,
        "the router provider API key should live only in the host credential store");
  const auto router_listed = router.invoke("provider_list", json::object());
  check(router_listed.ok && router_listed.value.at("providers").size() == 1,
        "provider_list should read the credential-backed provider");
  const auto router_deleted =
      router.invoke("provider_delete", {{"providerId", router_id}});
  check(router_deleted.ok && router_deleted.value.value("credentialReclaimed", false),
        "provider_delete should reclaim the router provider credential");

  // Agent run forwarding: the sidecar owns admission, stop and approval state,
  // so the router must relay the three commands instead of returning
  // NOT_IMPLEMENTED.
  (void)database.create_chat_session("ses_router", std::nullopt, std::nullopt,
      "Router admission", "2026-10-17T00:00:00Z");
  const auto admitted = router.invoke(
      "agent_run_start",
      {{"runId", "run_router_admission"}, {"sessionId", "ses_router"},
       {"prompt", "Summarize the current server health."}, {"messageId", "msg_router"},
       {"delivery", "async"}, {"resume", false}});
  check(admitted.ok && admitted.value.value("runId", std::string()) ==
                           "run_router_admission" &&
            admitted.value.value("started", true) == false,
        "agent_run_start should admit a message without executing it");

  const auto missing_provider = router.invoke(
      "agent_run_start",
      {{"runId", "run_router_execute"}, {"sessionId", "ses_router"},
       {"prompt", "Summarize the current server health."}, {"messageId", "msg_router"},
       {"delivery", "async"}, {"resume", true}});
  check(!missing_provider.ok && missing_provider.value.value("code", std::string()) ==
                                   "INVALID_PARAMS",
        "agent_run_start should surface the Agent's missing-provider error");

  const auto stopped = router.invoke("agent_run_stop", {{"runId", "run_router_missing"}});
  check(stopped.ok && stopped.value.value("stopped", true) == false,
        "agent_run_stop should forward and report an unknown run as not stopped");

  // The host only forwards an approval response for an id it tracked itself or
  // saw the sidecar announce. An id nobody ever requested is refused outright, so
  // the UI can never render a success the host never obtained, and the sidecar
  // never receives a response for an approval that is not in flight.
  const auto approval = router.invoke(
      "agent_approval_respond",
      {{"approvalId", "apr_router_missing"}, {"runId", "run_router_execute"},
       {"decision", "reject"}, {"respondedAt", "2026-10-16T00:00:00Z"}});
  check(!approval.ok &&
            approval.value.value("code", std::string()) == "AGENT_APPROVAL_UNKNOWN",
        "agent_approval_respond should refuse an approval the host never saw");
  check(!approval.value.contains("accepted"),
        "a refused approval must not report an acceptance to the UI");

  // Durable investigation start: the host binds a run to the task before it
  // forwards `agent.run.start`, so a rejected start must fail the bound run and
  // the task rather than leaving an active run behind.
  {
    const json probe_task = {
        {"id", "itask_sidecar"},
        {"objective", "Probe the local host"},
        {"successCriteria", json::array({"record a sample"})},
        {"scope", {{"host", "local"}, {"environment", "local"}}},
        {"guardrails",
         {{"forbiddenTools", json::array()}, {"forbiddenPathPrefixes", json::array()}}},
        {"mode", "readonly"},
        {"permissionMode", "ask"},
        {"automationLevel", "readonly"},
        {"phase", "investigating"},
        {"status", "pending"},
        {"budget", {{"maxSteps", 4}, {"maxRunMs", 30000}, {"maxAttempts", 2}}},
        {"createdBy", "user"},
        {"createdAt", "2026-01-05T00:00:00Z"},
        {"updatedAt", "2026-01-05T00:00:00Z"},
    };
    (void)database.create_investigation_task(probe_task);
    const auto start = router.invoke("investigation_task_start", {{"taskId", "itask_sidecar"}});
    check(!start.ok && start.value.value("code", std::string()) == "INVALID_PARAMS" &&
              start.value.value("message", std::string()).find("No enabled AI provider") != std::string::npos,
          "a first task must hydrate its real conversation before reaching the missing-provider check");
    const auto task_chat = database.get_chat_session("session_task_itask_sidecar");
    check(!task_chat.at("session").contains("serverId") && task_chat.at("messages").size() == 1 &&
              task_chat.at("messages").at(0).at("role") == "user" &&
              task_chat.at("messages").at(0).at("content").get<std::string>().find("Probe the local host") != std::string::npos,
          "the initial task must save its real scoped prompt before asking Python to hydrate");
    const auto detail = database.get_investigation_task("itask_sidecar");
    check(detail.has_value() && detail->at("task").at("status") == "failed" &&
              detail->at("runs").size() == 1 &&
              detail->at("runs").at(0).at("status") == "failed" &&
              detail->at("runs").at(0).at("failure").at("code") == "transport",
          "a rejected Agent start should fail the bound run and the task");
  }

  const auto killed = router.invoke("agent_kill", json::object());
  check(killed.ok && killed.value.value("killed", false),
        "agent_kill should stop the running sidecar");

  // A tiny sidecar relay exercises the router's real private host RPC handler
  // without a model, provider, or exposing a test-only production entry point.
  const auto relay_source = directory.path / "context-relay";
  std::filesystem::create_directories(relay_source / "yukinal_agent");
  std::ofstream(relay_source / "yukinal_agent" / "__init__.py").close();
  {
    std::ofstream script(relay_source / "yukinal_agent" / "sidecar.py");
    script << R"PY(import json
import os
import sys

def send(frame):
    print(json.dumps(frame, separators=(",", ":")), flush=True)

for line in sys.stdin:
    frame = json.loads(line)
    method = frame.get("method")
    if method == "initialize":
        result = {"protocolVersion": "1.0", "agentVersion": "context-relay"}
    elif method == "tools.list":
        result = {"tools": []}
    elif method == "provider.list":
        # Announce a sidecar-owned approval (a non-host-action tool) on the same
        # notification stream the ledger observes, then answer the host request.
        send({"jsonrpc": "2.0", "method": "agent.stream", "params": {
            "type": "agent.waiting_approval",
            "runId": "run_relay",
            "approval": {"approvalId": "apr_relay_foreign", "runId": "run_relay",
                         "traceId": "trace_relay", "callId": "call_relay",
                         "toolName": "filesystem.write",
                         "input": {"path": "/tmp/relay"},
                         "inputFingerprint": "0" * 64,
                         "target": {"host": "local", "environment": "local"},
                         "planId": "plan_relay", "planStepId": "step_relay",
                         "evidenceIds": [],
                         "expiresAt": "2099-01-01T00:00:00Z"}}})
        send({"jsonrpc": "2.0", "id": 9000, "method": "host.chat.context", "params": frame["params"]})
        response = json.loads(sys.stdin.readline())
        if "error" in response:
            send({"jsonrpc": "2.0", "id": frame["id"], "error": response["error"]})
            continue
        result = response["result"]
    elif method == "agent.approval.respond":
        params = frame.get("params") or {}
        # Record every decision that actually reaches the sidecar: the host layer
        # is the guard that must keep a decision it cannot grant away from here.
        with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "approvals.log"),
                  "a", encoding="utf-8") as handle:
            handle.write(str(params.get("decision")) + "\n")
        result = {"accepted": params.get("approvalId") == "apr_relay_foreign"}
    else:
        send({"jsonrpc": "2.0", "id": frame["id"], "error": {"code": -32601, "message": "unknown relay method"}})
        continue
    send({"jsonrpc": "2.0", "id": frame["id"], "result": result})
)PY";
  }
#ifdef _WIN32
  SetEnvironmentVariableA("YUKINAL_AGENT_SOURCE", relay_source.string().c_str());
#else
  setenv("YUKINAL_AGENT_SOURCE", relay_source.string().c_str(), 1);
#endif
  (void)database.create_chat_session("ses_hydration", std::nullopt, std::nullopt,
      "Hydration snapshot", "2026-10-17T01:00:00Z");
  const auto stamp = "2026-10-17T01:00:01Z";
  const json actor = {{"id", "reviewer"}, {"label", "Reviewer"}, {"kind", "agent"}};
  (void)database.append_chat_message("z_prior", "ses_hydration", "user", "Earlier question",
      std::nullopt, std::nullopt, stamp);
  (void)database.append_chat_message("y_answer", "ses_hydration", "assistant", "Earlier answer",
      std::nullopt, std::nullopt, stamp, actor, std::string("recorded-model"));
  const json large_attachment = json::array({{{"type", "image"},
      {"data", std::string(600 * 1024, 'A')}, {"mediaType", "image/png"}}});
  (void)database.append_chat_message("x_attachment", "ses_hydration", "user", "Attachment question",
      large_attachment, std::string("trace_attachment"), stamp);
  (void)database.append_chat_message("a_current", "ses_hydration", "user", "Current question",
      std::nullopt, std::nullopt, stamp);
  (void)database.append_chat_message("0_future", "ses_hydration", "assistant", "Future answer",
      std::nullopt, std::nullopt, stamp);
  yukinal::desktop_host::IpcRouter context_router(&database, &credentials);
  check(context_router.invoke("agent_spawn", json::object()).ok,
        "the context relay sidecar should start");
  const json context_params = {{"sessionId", "ses_hydration"}, {"excludeMessageId", "a_current"}, {"limit", 1000}};
  const auto hydrated = context_router.invoke("provider_list", context_params);
  check(hydrated.ok && hydrated.value.at("messages").size() == 3 &&
        !hydrated.value.at("truncated").get<bool>(),
        "attachment bytes must not discard text or earlier hydration messages");
  const auto& history = hydrated.value.at("messages");
  check(history.at(0).at("id") == "z_prior" && history.at(1).at("id") == "y_answer" &&
        history.at(2).at("id") == "x_attachment",
        "same-second cutoff must retain earlier turns and exclude current/future messages");
  check(history.at(1).at("participant") == actor && history.at(1).at("model") == "recorded-model" &&
        history.at(1).at("createdAt") == stamp,
        "hydration must retain actor, model, and display time metadata");
  check(!history.at(2).contains("parts") && !history.at(2).contains("traceId") &&
        !history.at(2).contains("sessionId") && history.at(2).size() == 4,
        "hydration must project text fields before enforcing its byte limit");
  auto limited_params = context_params;
  limited_params["limit"] = 2;
  const auto limited_history = context_router.invoke("provider_list", limited_params);
  check(limited_history.ok && limited_history.value.at("messages").size() == 2 &&
        limited_history.value.at("messages").at(0).at("id") == "y_answer" &&
        limited_history.value.at("truncated").get<bool>(),
        "bounded hydration must select the newest earlier text messages in append order");
  auto foreign_params = context_params;
  foreign_params["serverId"] = "srv_other";
  const auto foreign_history = context_router.invoke("provider_list", foreign_params);
  check(!foreign_history.ok && foreign_history.value.value("code", std::string()) == "INVALID_PARAMS",
        "the native hydration RPC must reject another server's history");
  // The other half of the unknown-id rule, end to end: the relay announced a
  // sidecar-owned approval (`filesystem.write`, not a host action) on the real
  // notification stream, so a response to it must still be forwarded and must
  // report the sidecar's own answer. Without this, "refuse unknown ids" could
  // silently degrade into "refuse every sidecar-owned approval".
  const auto relayed = context_router.invoke(
      "agent_approval_respond",
      {{"approvalId", "apr_relay_foreign"}, {"runId", "run_relay"},
       {"decision", "approve_once"}, {"respondedAt", "2026-10-17T01:00:02Z"}});
  check(relayed.ok && relayed.value.value("accepted", false),
        "an observed sidecar-owned approval must still forward and report accepted");
  // ...and a decision the host cannot grant is refused before that forward, so a
  // session grant never reaches the sidecar as if it were an approval.
  for (const char* decision : {"approve_session", "maybe"}) {
    const auto refused = context_router.invoke(
        "agent_approval_respond",
        {{"approvalId", "apr_relay_foreign"}, {"runId", "run_relay"},
         {"decision", decision}, {"respondedAt", "2026-10-17T01:00:02Z"}});
    check(!refused.ok && refused.value.value("code", std::string()) == "INVALID_PARAMS",
          "a decision outside the whitelist must be refused at the host layer");
  }
  // The relay logged every decision that actually reached the sidecar: only the
  // forwarded `approve_once` may be there, because the host answers the RPC after
  // handling it. A refused decision must never have crossed the pipe.
  {
    std::ifstream decisions(relay_source / "yukinal_agent" / "approvals.log");
    std::string line;
    std::vector<std::string> seen;
    while (std::getline(decisions, line)) {
      if (!line.empty()) seen.push_back(line);
    }
    check(seen == std::vector<std::string>{"approve_once"},
          "only a whitelisted decision may reach the sidecar");
  }
  check(context_router.invoke("agent_kill", json::object()).ok,
        "the context relay sidecar should stop cleanly");

  // REGRESSION TEST: a timed-out `agent.run.*` request must cancel the run in
  // the sidecar. Before the fix, `request()` erased the pending entry and threw
  // while the Python run kept burning its budget with a result nobody could
  // receive. The fake sidecar records every frame it receives; it deliberately
  // never answers `agent.run.start`.
  {
    const auto timeout_source = directory.path / "timeout-probe";
    std::filesystem::create_directories(timeout_source / "yukinal_agent");
    std::ofstream(timeout_source / "yukinal_agent" / "__init__.py").close();
    {
      std::ofstream script(timeout_source / "yukinal_agent" / "sidecar.py");
      script << R"PY(import json
import os
import sys

log_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "frames.log")

def record(frame):
    with open(log_path, "a", encoding="utf-8") as handle:
        handle.write(json.dumps({"method": frame.get("method"),
                                 "params": frame.get("params"),
                                 "hasId": "id" in frame},
                                separators=(",", ":")) + "\n")

def send(frame):
    print(json.dumps(frame, separators=(",", ":")), flush=True)

for line in sys.stdin:
    frame = json.loads(line)
    method = frame.get("method")
    record(frame)
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": frame["id"],
              "result": {"protocolVersion": "1.0", "agentVersion": "timeout-probe"}})
    elif method == "tools.list":
        send({"jsonrpc": "2.0", "id": frame["id"], "result": {"tools": []}})
    elif method == "agent.run.start":
        # Simulate a run that is still working: answer nothing at all.
        pass
    elif method == "agent.run.stop":
        send({"jsonrpc": "2.0", "id": frame["id"], "result": {"stopped": True}})
    else:
        send({"jsonrpc": "2.0", "id": frame["id"],
              "error": {"code": -32601, "message": "unknown timeout-probe method"}})
)PY";
    }
    SidecarClient timeout_client;
    SidecarLaunch timeout_launch;
    timeout_launch.program = argv[1];
    timeout_launch.args = {"-m", "yukinal_agent.sidecar"};
    timeout_launch.environment.push_back({"PYTHONPATH", timeout_source.string()});
    timeout_launch.data_directory = (timeout_source / "data").string();
    timeout_launch.client_version = "timeout-probe";
    timeout_client.configure(timeout_launch);
    timeout_client.set_host_handler(handle_host);
    const auto timeout_started = timeout_client.start(std::chrono::milliseconds(20000));
    check(timeout_started.pid != 0, "the timeout probe sidecar should start");
    bool timed_out = false;
    const auto request_started = std::chrono::steady_clock::now();
    try {
      (void)timeout_client.request("agent.run.start", {{"runId", "run_timeout_probe"}},
                                   std::chrono::milliseconds(300));
    } catch (const yukinal::desktop_host::SidecarError&) {
      timed_out = true;
    }
    check(timed_out, "an unanswered agent.run.start must time out");
    check(std::chrono::steady_clock::now() - request_started < std::chrono::seconds(10),
          "a timed-out run request must not block on run cleanup indefinitely");
    // The cleanup is intentionally best-effort and asynchronous from this
    // thread's point of view, so poll the sidecar's record for a bounded while.
    bool saw_stop = false;
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(5);
    while (std::chrono::steady_clock::now() < deadline) {
      std::ifstream log(timeout_source / "yukinal_agent" / "frames.log");
      std::string line;
      while (std::getline(log, line)) {
        const auto entry = json::parse(line, nullptr, false);
        if (entry.is_discarded() || !entry.is_object()) continue;
        if (entry.value("method", std::string()) == "agent.run.stop" &&
            entry.value("hasId", false) && entry.contains("params") &&
            entry.at("params").is_object() &&
            entry.at("params").value("runId", std::string()) == "run_timeout_probe") {
          saw_stop = true;
        }
      }
      if (saw_stop) break;
      std::this_thread::sleep_for(std::chrono::milliseconds(50));
    }
    check(saw_stop,
          "a timed-out agent.run.start must send agent.run.stop for the same run");
    timeout_client.stop(std::chrono::milliseconds(5000));
  }

  return 0;
}

int main(int argc, char** argv) {
#ifndef _WIN32
  // This executable exercises the pipe-failure path handled by the desktop
  // host's POSIX entry point.
  std::signal(SIGPIPE, SIG_IGN);
#endif
  try {
    return run_tests(argc, argv);
  } catch (const std::exception& error) {
    std::cerr << "[sidecar-test] fatal: " << error.what() << '\n';
    return 1;
  } catch (...) {
    std::cerr << "[sidecar-test] fatal: unknown exception\n";
    return 1;
  }
}

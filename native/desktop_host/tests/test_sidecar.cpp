#include "yukinal/desktop_host/credential_store.hpp"
#include "yukinal/desktop_host/database.hpp"
#include "yukinal/desktop_host/ipc_router.hpp"
#include "yukinal/desktop_host/sidecar.hpp"

#include <chrono>
#include <cstddef>
#include <cstdlib>
#include <filesystem>
#include <iostream>
#include <map>
#include <string>
#include <string_view>

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

  auto handle_host = [&credentials](const std::string& method, const json& params) -> json {
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

  const auto approval = router.invoke(
      "agent_approval_respond",
      {{"approvalId", "apr_router_missing"}, {"runId", "run_router_execute"},
       {"decision", "reject"}, {"respondedAt", "2026-10-16T00:00:00Z"}});
  check(approval.ok && approval.value.value("accepted", true) == false,
        "agent_approval_respond should forward and report an unknown approval as stale");

  const auto killed = router.invoke("agent_kill", json::object());
  check(killed.ok && killed.value.value("killed", false),
        "agent_kill should stop the running sidecar");

  return 0;
}

int main(int argc, char** argv) {
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

#include "yukinal/desktop_host/credential_store.hpp"
#include "yukinal/desktop_host/database.hpp"
#include "yukinal/desktop_host/desktop_mcp.hpp"

#include <chrono>
#include <filesystem>
#include <fstream>
#include <future>
#include <iostream>
#include <iterator>
#include <latch>
#include <stdexcept>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#else
#include <unistd.h>
#endif

namespace {

using namespace std::chrono_literals;
using Json = nlohmann::json;
using yukinal::desktop_host::DesktopDatabase;
using yukinal::desktop_host::McpSupervisor;

void check(bool condition, const char* message) {
  if (!condition) throw std::runtime_error(message);
}

template <typename Predicate>
bool wait_until(Predicate predicate, std::chrono::milliseconds timeout = 2500ms) {
  const auto deadline = std::chrono::steady_clock::now() + timeout;
  do {
    if (predicate()) return true;
    std::this_thread::sleep_for(5ms);
  } while (std::chrono::steady_clock::now() < deadline);
  return predicate();
}

void mark(const std::filesystem::path& path) {
  std::filesystem::create_directories(path.parent_path());
  std::ofstream stream(path);
  stream << "fixture\n";
}

std::size_t count(const std::filesystem::path& path) {
  if (!std::filesystem::exists(path)) return 0;
  return static_cast<std::size_t>(std::distance(std::filesystem::directory_iterator(path),
                                               std::filesystem::directory_iterator()));
}

// This executable is the only child ever launched. All coordination uses files
// below its own temporary directory; no network or OS credential store is used.
int fixture(const std::filesystem::path& control) {
#ifdef _WIN32
  const auto pid = std::to_string(GetCurrentProcessId());
#else
  const auto pid = std::to_string(getpid());
#endif
  mark(control / "started" / pid);
  struct ExitMarker {
    std::filesystem::path path;
    ~ExitMarker() { mark(path); }
  } exit_marker{control / "exited" / pid};
  std::string line;
  while (std::getline(std::cin, line)) {
    const auto frame = Json::parse(line, nullptr, false);
    if (!frame.is_object() || !frame.contains("id")) continue;
    const auto method = frame.value("method", std::string());
    Json response = {{"jsonrpc", "2.0"}, {"id", frame.at("id")}};
    bool exit_after_response = false;
    if (method == "initialize") {
      mark(control / "initializing" / pid);
      if (!wait_until([&] { return std::filesystem::exists(control / "release"); }, 5s)) {
        return 2;
      }
      if (std::filesystem::exists(control / "reject")) {
        response["error"] = {{"code", -32000}, {"message", "fixture handshake rejected"}};
        exit_after_response = true;
      } else {
        response["result"] = {{"protocolVersion", "2024-11-05"},
                              {"capabilities", Json::object()},
                              {"serverInfo", {{"name", "lifecycle-fixture"}, {"version", "1"}}}};
      }
    } else if (method == "tools/list") {
      response["result"] = {
          {"tools", Json::array({{{"name", "echo"}, {"inputSchema", {{"type", "object"}}}},
                                 {{"name", "exit"}, {"inputSchema", {{"type", "object"}}}}})}};
    } else if (method == "tools/call") {
      exit_after_response = frame.at("params").value("name", std::string()) == "exit";
      response["result"] = {{"content", Json::array({{{"type", "text"}, {"text", pid}}})},
                            {"isError", false}};
    } else {
      response["error"] = {{"code", -32601}, {"message", "not found"}};
    }
    std::cout << response.dump() << '\n' << std::flush;
    if (exit_after_response) return 0;
  }
  if (std::filesystem::exists(control / "hold_stop")) {
    mark(control / "closing" / pid);
    if (!wait_until([&] { return std::filesystem::exists(control / "exit_release"); }, 5s)) {
      return 3;
    }
  }
  return 0;
}

class NoCredentials final : public yukinal::desktop_host::CredentialStore {
 public:
  yukinal::desktop_host::CredentialReference set(
      const std::string&, const std::string&, const yukinal::desktop_host::Secret&) const override {
    throw std::runtime_error("the fixture must not access credentials");
  }
  yukinal::desktop_host::Secret get(
      const yukinal::desktop_host::CredentialReference&) const override {
    throw std::runtime_error("the fixture must not access credentials");
  }
  bool has(const yukinal::desktop_host::CredentialReference&) const override {
    throw std::runtime_error("the fixture must not access credentials");
  }
  void erase(const yukinal::desktop_host::CredentialReference&) const override {
    throw std::runtime_error("the fixture must not access credentials");
  }
};

struct TemporaryDirectory {
  std::filesystem::path path = std::filesystem::temp_directory_path() /
      ("yukinal-mcp-lifecycle-" + std::to_string(
           std::chrono::steady_clock::now().time_since_epoch().count()));
  ~TemporaryDirectory() {
    std::error_code error;
    std::filesystem::remove_all(path, error);
  }
};

struct World {
  TemporaryDirectory temporary;
  DesktopDatabase database{temporary.path / "database"};
  NoCredentials credentials;
  McpSupervisor supervisor{&database, &credentials};
  std::filesystem::path executable;

  explicit World(std::filesystem::path executable) : executable(std::move(executable)) {}

  std::filesystem::path add(const std::string& id, bool released = false) {
    const auto control = temporary.path / id;
    std::filesystem::create_directories(control);
    if (released) mark(control / "release");
    database.upsert_mcp_server({{"id", id}, {"label", id}, {"transport", "stdio"},
                                {"command", executable.string()},
                                {"args", Json::array({"--fixture", control.string()})},
                                {"enabled", true}, {"allowedTools", Json::array({"echo", "exit"})},
                                {"trustLevel", "reviewed"}, {"annotationTrust", "none"}});
    return control;
  }
};

void concurrent_start_and_catalog(const std::filesystem::path& executable) {
  World world(executable);
  const auto control = world.add("shared");
  constexpr int callers = 8;
  std::latch ready(callers), go(1);
  std::vector<std::future<Json>> requests;
  for (int index = 0; index < callers; ++index) {
    requests.push_back(std::async(std::launch::async, [&, index] {
      ready.count_down();
      go.wait();
      return index % 2 == 0 ? world.supervisor.start("shared") : world.supervisor.catalog();
    }));
  }
  ready.wait();
  go.count_down();
  const bool initializing = wait_until([&] { return count(control / "initializing") != 0; });
  std::this_thread::sleep_for(200ms);
  mark(control / "release");
  for (int index = 0; index < callers; ++index) {
    const auto result = requests[index].get();
    if (index % 2 == 0) check(result.at("status").at("running") == true, "start failed");
    else check(result.at("servers").size() == 1, "catalog did not share the running session");
  }
  check(initializing, "fixture did not initialize");
  check(count(control / "started") == 1, "concurrent start/catalog launched duplicate children");
  check(world.supervisor.call("mcp.shared.echo", Json::object()).at("isError") == false,
        "the shared child was not callable");
  check(world.supervisor.stop("shared").at("shutdown").at("wasRunning") == true,
        "stop did not find the shared child");
  check(count(control / "exited") == 1, "stop did not reap the shared child");
}

void independent_servers(const std::filesystem::path& executable) {
  World world(executable);
  const auto held = world.add("held");
  world.add("fast", true);
  auto first = std::async(std::launch::async, [&] { return world.supervisor.start("held"); });
  const bool initializing = wait_until([&] { return count(held / "initializing") == 1; });
  auto second = std::async(std::launch::async, [&] { return world.supervisor.start("fast"); });
  const bool independent = second.wait_for(1500ms) == std::future_status::ready;
  mark(held / "release");
  (void)first.get();
  (void)second.get();
  (void)world.supervisor.stop("held");
  (void)world.supervisor.stop("fast");
  check(initializing, "the held fixture did not initialize");
  check(independent, "one server's handshake blocked another server");
}

void failed_start_is_shared_and_retryable(const std::filesystem::path& executable) {
  World world(executable);
  const auto control = world.add("retry");
  mark(control / "reject");
  constexpr int callers = 8;
  std::latch ready(callers), go(1);
  std::vector<std::future<bool>> requests;
  for (int index = 0; index < callers; ++index) {
    requests.push_back(std::async(std::launch::async, [&] {
      ready.count_down();
      go.wait();
      try {
        (void)world.supervisor.start("retry");
        return false;
      } catch (const std::exception& error) {
        return std::string(error.what()) == "fixture handshake rejected";
      }
    }));
  }
  ready.wait();
  go.count_down();
  const bool initializing = wait_until([&] { return count(control / "initializing") != 0; });
  std::this_thread::sleep_for(200ms);
  mark(control / "release");
  bool all_failed = true;
  for (auto& request : requests) all_failed = request.get() && all_failed;
  check(initializing && all_failed, "concurrent callers did not receive the startup failure");
  check(count(control / "started") == 1, "one failed attempt was launched more than once");
  check(world.supervisor.stop("retry").at("shutdown").at("wasRunning") == false,
        "a failed attempt remained in the session cache");
  std::filesystem::remove(control / "reject");
  check(world.supervisor.start("retry").at("status").at("running") == true,
        "a failed start prevented retry");
  check(count(control / "started") == 2, "retry did not start exactly one fresh child");
  (void)world.supervisor.stop("retry");
}

void stop_during_start(const std::filesystem::path& executable) {
  World world(executable);
  const auto control = world.add("starting");
  auto start = std::async(std::launch::async, [&] { return world.supervisor.start("starting"); });
  const bool initializing = wait_until([&] { return count(control / "initializing") == 1; });
  auto stop = std::async(std::launch::async, [&] { return world.supervisor.stop("starting"); });
  const bool waited = stop.wait_for(100ms) == std::future_status::timeout;
  mark(control / "release");
  (void)start.get();
  const auto stopped = stop.get();
  check(initializing && waited, "stop returned while startup was still in flight");
  check(stopped.at("shutdown").at("wasRunning") == true,
        "stop did not remove the child published by the in-flight start");
  check(count(control / "exited") == 1, "the in-flight child survived stop");
  check(world.supervisor.stop("starting").at("shutdown").at("wasRunning") == false,
        "stop should remain idempotent");
  check(world.supervisor.start("starting").at("status").at("running") == true,
        "restart after a concurrent stop failed");
  check(count(control / "started") == 2, "restart launched an unexpected number of children");
  (void)world.supervisor.stop("starting");
}

void restart_waits_for_stop_cleanup(const std::filesystem::path& executable) {
  World world(executable);
  const auto control = world.add("stopping", true);
  mark(control / "hold_stop");
  (void)world.supervisor.start("stopping");
  auto stop = std::async(std::launch::async, [&] { return world.supervisor.stop("stopping"); });
  const bool closing = wait_until([&] { return count(control / "closing") == 1; });
  auto restart = std::async(std::launch::async, [&] { return world.supervisor.start("stopping"); });
  const bool waited = restart.wait_for(100ms) == std::future_status::timeout;
  const auto children_before_cleanup = count(control / "started");
  mark(control / "exit_release");
  (void)stop.get();
  const auto restarted = restart.get();
  (void)world.supervisor.stop("stopping");
  check(closing && waited && children_before_cleanup == 1,
        "restart overlapped the previous child's cleanup");
  check(restarted.at("status").at("running") == true && count(control / "started") == 2,
        "restart after cleanup failed");
  check(count(control / "exited") == 2, "restart left an unreaped fixture child");
}

void exited_child_can_restart(const std::filesystem::path& executable) {
  World world(executable);
  const auto control = world.add("exited", true);
  (void)world.supervisor.start("exited");
  (void)world.supervisor.call("mcp.exited.exit", Json::object());
  const bool exited = wait_until([&] { return count(control / "exited") == 1; });
  const bool restarted = wait_until([&] {
    (void)world.supervisor.start("exited");
    return count(control / "started") == 2;
  });
  check(exited && restarted, "an exited child's cached session prevented restart");
  check(world.supervisor.call("mcp.exited.echo", Json::object()).at("isError") == false,
        "the replacement child was not callable");
  (void)world.supervisor.stop("exited");
  check(count(control / "exited") == 2, "the exited session's threads were not cleaned up");
}

}  // namespace

int main(int argc, char** argv) {
  if (argc == 3 && std::string(argv[1]) == "--fixture") return fixture(argv[2]);
  try {
    check(argc > 0, "test executable path is required");
    const auto executable = std::filesystem::absolute(argv[0]);
    concurrent_start_and_catalog(executable);
    independent_servers(executable);
    failed_start_is_shared_and_retryable(executable);
    stop_during_start(executable);
    restart_waits_for_stop_cleanup(executable);
    exited_child_can_restart(executable);
    std::cout << "6 MCP lifecycle tests passed\n";
    return 0;
  } catch (const std::exception& error) {
    std::cerr << error.what() << '\n';
    return 1;
  }
}

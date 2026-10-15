#include "yukinal/desktop_host/ipc_router.hpp"

#include <cstdlib>
#include <iostream>

namespace {

void check(bool condition, const char* message) {
  if (condition) return;
  std::cerr << message << '\n';
  std::exit(1);
}

}  // namespace

int main() {
  using nlohmann::json;
  using yukinal::desktop_host::IpcRouter;

  IpcRouter router;
  const auto ping = router.invoke("core_ping", json::object());
  check(ping.ok, "core_ping should succeed");
  check(ping.value.value("version", "") == YUKINAL_DESKTOP_VERSION,
        "core_ping should report the app version");
  check(ping.value.contains("os"), "core_ping should report the platform");

  check(!router.invoke("core_ping", {{"unexpected", true}}).ok,
        "core_ping should reject unexpected parameters");
  check(!router.invoke("server_list", json::object()).ok,
        "server_list should report an unavailable database");
  check(!router.invoke("activity_list", {{"limit", 101}}).ok,
        "activity_list should reject limits greater than 100");
  check(!router.invoke("activity_list", {{"serverId", "not-a-server-id"}}).ok,
        "activity_list should reject malformed server ids");
  check(!router.invoke("activity_list", json::object()).ok,
        "activity_list should report a missing local database");
  check(!router.invoke("shell.open_url", {{"url", "javascript:alert(1)"}}).ok,
        "external URL opening should reject executable schemes");
  check(yukinal::desktop_host::is_external_url("https://example.com/path"),
        "HTTPS URLs should be accepted");
  check(yukinal::desktop_host::is_external_url("mailto:ops@example.com"),
        "mailto URLs should be accepted");
  check(!yukinal::desktop_host::is_external_url("https://example.com\n"),
        "external URLs should reject control characters");

  const auto subscription = router.invoke("__events.subscribe",
      {{"event", "agent.completed"}, {"token", "01234567-89ab-cdef-0123-456789abcdef"}});
  check(subscription.ok, "valid event subscriptions should succeed");
  check(router.subscribers("agent.completed").size() == 1,
        "subscribers should be tracked by event name");
  check(!router.invoke("__events.subscribe",
      {{"event", "agent.completed"}, {"token", "short"}}).ok,
      "invalid subscription tokens should be rejected");
  const auto unsubscribe = router.invoke("__events.unsubscribe",
      {{"event", "agent.completed"}, {"token", "01234567-89ab-cdef-0123-456789abcdef"}});
  check(unsubscribe.ok && router.subscribers("agent.completed").empty(),
        "event unsubscription should release its callback token");
  return 0;
}

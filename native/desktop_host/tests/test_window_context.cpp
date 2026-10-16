#include "yukinal/desktop_host/window_context.hpp"
#include "yukinal/desktop_host/ipc_router.hpp"

#include <cstdlib>
#include <iostream>
#include <vector>

namespace {
void check(bool condition, const char* message) {
  if (condition) return;
  std::cerr << message << '\n';
  std::exit(1);
}
}  // namespace

int main() {
  using nlohmann::json;
  using yukinal::desktop_host::window_context_label;
  const json base{{"section", "servers"}, {"serverName", nullptr},
                  {"serverStatus", nullptr}, {"agentBusy", false}};
  for (const auto* section : {"servers", "agents", "projects", "tasks", "activity", "settings"}) {
    auto context = base;
    context["section"] = section;
    check(window_context_label(context).has_value(), "all workspace sections should be accepted");
  }
  for (const auto* status : {"connected", "connecting", "disconnected", "error"}) {
    auto context = base;
    context["serverStatus"] = status;
    check(window_context_label(context).has_value(), "all server statuses should be accepted");
  }
  auto context = base;
  context["serverName"] = "Test server";
  context["serverStatus"] = "connected";
  context["agentBusy"] = true;
  check(window_context_label(context) == L"\u670d\u52a1\u5668  /  Test server  \u00b7  \u5df2\u8fde\u63a5  \u00b7  Agent \u8fd0\u884c\u4e2d",
        "the caption should reflect selected server, status, and Agent work");
  context["section"] = "settings";
  check(window_context_label(context) == L"\u8bbe\u7f6e  \u00b7  Agent \u8fd0\u884c\u4e2d",
        "other sections should not show stale selected-server context");
  context["section"] = "agents";
  check(window_context_label(context) == L"Agent NIST  \u00b7  Agent \u8fd0\u884c\u4e2d",
        "Agent NIST should retain busy state without stale server identity");
  context["agentBusy"] = false;
  check(window_context_label(context) == L"Agent NIST",
        "the agents section should use the Agent NIST caption");

  std::vector<json> invalid{nullptr, json::array(), json::object()};
  for (const auto* field : {"section", "serverName", "serverStatus", "agentBusy"}) {
    auto missing = base;
    missing.erase(field);
    invalid.push_back(missing);
    auto wrong_type = base;
    wrong_type[field] = 42;
    invalid.push_back(wrong_type);
  }
  for (const auto& [field, value] : std::vector<std::pair<std::string, json>>{
      {"section", "terminal"}, {"serverStatus", "unknown"}, {"agentBusy", "true"},
      {"serverName", "line\nbreak"}, {"serverName", std::string("nul\0name", 8)},
      {"serverName", std::string(1, '\x7f')}, {"serverName", std::string(161, 'a')},
      {"serverName", std::string(641, 'a')}, {"serverName", std::string("\xc0\x80", 2)},
      {"serverName", std::string("\xed\xa0\x80", 3)}, {"approved", true},
      {"apiKey", "test-only"}, {"command", "server_connect"}}) {
    auto value_context = base;
    value_context[field] = value;
    invalid.push_back(value_context);
  }
  for (const auto& value : invalid) {
    check(!window_context_label(value), "malformed or authority-bearing context should be rejected");
  }
  context = base;
  std::string emoji;
  for (int i = 0; i < 80; ++i) emoji += "\xf0\x9f\x9b\xb0";
  context["serverName"] = emoji;
  check(window_context_label(context).has_value(), "160 UTF-16 units should fit, including surrogate pairs");
  context["serverName"] = emoji + "a";
  check(!window_context_label(context), "the name limit should match the TypeScript UTF-16 bound");

  yukinal::desktop_host::IpcRouter router;
  check(!router.invoke("__shell.update_context", base).ok,
        "the shell presentation channel must not enter the business command router");
  return 0;
}

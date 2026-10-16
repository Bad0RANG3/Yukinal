#include "yukinal/desktop_host/window_context.hpp"

#include <windows.h>
#include <algorithm>
#include <unordered_set>

namespace yukinal::desktop_host {

namespace {
std::optional<std::wstring> shell_text(const nlohmann::json& value, int maximum) {
  if (!value.is_string()) return std::nullopt;
  const auto utf8 = value.get<std::string>();
  if (utf8.size() > static_cast<size_t>(maximum * 4) || utf8.find('\0') != std::string::npos) return std::nullopt;
  const int length = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, utf8.data(),
      static_cast<int>(utf8.size()), nullptr, 0);
  if (length > maximum || (length == 0 && !utf8.empty())) return std::nullopt;
  std::wstring text(static_cast<size_t>(length), L'\0');
  if (length > 0) MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, utf8.data(),
      static_cast<int>(utf8.size()), text.data(), length);
  for (const wchar_t ch : text) if (ch < 32 || ch == 127) return std::nullopt;
  return text;
}
bool valid_server_id(const nlohmann::json& value) {
  if (!value.is_string()) return false;
  const auto id = value.get<std::string>();
  return id.starts_with("srv_") && id.size() > 4 && id.size() <= 256 &&
      std::all_of(id.begin() + 4, id.end(), [](unsigned char c) { return (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9'); });
}
bool valid_status(const nlohmann::json& value) {
  return value == "connected" || value == "connecting" || value == "disconnected" || value == "error";
}
}  // namespace

std::optional<std::wstring> window_context_label(const nlohmann::json& params) {
  if (!params.is_object() || params.size() != 4 || !params.contains("section") ||
      !params.contains("serverName") || !params.contains("serverStatus") ||
      !params.contains("agentBusy") || !params["section"].is_string() ||
      !params["agentBusy"].is_boolean()) return std::nullopt;
  const auto section = params["section"].get<std::string>();
  std::wstring label;
  if (section == "servers") label = L"\u670d\u52a1\u5668";
  else if (section == "projects") label = L"\u9879\u76ee";
  else if (section == "tasks") label = L"\u6392\u67e5\u4efb\u52a1";
  else if (section == "activity") label = L"\u52a8\u6001";
  else if (section == "settings") label = L"\u8bbe\u7f6e";
  else return std::nullopt;
  const auto& name = params["serverName"];
  if (!name.is_null()) {
    const auto wide_name = shell_text(name, 160);
    if (!wide_name) return std::nullopt;
    if (section == "servers" && !wide_name->empty()) label += L"  /  " + *wide_name;
  }
  const auto& status = params["serverStatus"];
  if (!status.is_null()) {
    if (!status.is_string()) return std::nullopt;
    const auto value = status.get<std::string>();
    std::wstring status_label;
    if (value == "connected") status_label = L"\u5df2\u8fde\u63a5";
    else if (value == "connecting") status_label = L"\u8fde\u63a5\u4e2d";
    else if (value == "disconnected") status_label = L"\u672a\u8fde\u63a5";
    else if (value == "error") status_label = L"\u8fde\u63a5\u9519\u8bef";
    else return std::nullopt;
    if (section == "servers") label += L"  \u00b7  " + status_label;
  }
  if (params["agentBusy"].get<bool>()) label += L"  \u00b7  Agent \u8fd0\u884c\u4e2d";
  return label;
}

std::optional<WindowSidebar> parse_window_sidebar(const nlohmann::json& params) {
  if (!params.is_object() || params.size() != 5 || !params.contains("selectedServerId") ||
      !params.contains("servers") || !params["servers"].is_array() ||
      !params.contains("loading") || !params["loading"].is_boolean() ||
      !params.contains("failed") || !params["failed"].is_boolean() ||
      !params.contains("blocked") || !params["blocked"].is_boolean()) return std::nullopt;
  WindowSidebar sidebar;
  if (!params["selectedServerId"].is_null()) {
    if (!valid_server_id(params["selectedServerId"])) return std::nullopt;
    sidebar.selected_server_id = params["selectedServerId"].get<std::string>();
  }
  std::unordered_set<std::string> ids;
  for (const auto& row : params["servers"]) {
    if (!row.is_object() || row.size() != 4 || !row.contains("id") || !valid_server_id(row["id"]) ||
        !row.contains("name") || !row.contains("detail") || !row.contains("status") || !valid_status(row["status"])) return std::nullopt;
    const auto name = shell_text(row["name"], 160);
    const auto detail = shell_text(row["detail"], 256);
    const auto id = row["id"].get<std::string>();
    if (!name || !detail || !ids.insert(id).second) return std::nullopt;
    sidebar.servers.push_back({id, *name, *detail, row["status"].get<std::string>()});
  }
  sidebar.loading = params["loading"].get<bool>();
  sidebar.failed = params["failed"].get<bool>();
  sidebar.blocked = params["blocked"].get<bool>();
  return sidebar;
}

WindowLayout window_layout(int width, int height, int title_height, int inset, int sidebar_width) {
  const int available = (std::max)(1, width - inset);
  const int sidebar = (std::clamp)(sidebar_width, 0, available - 1);
  return {sidebar, sidebar, title_height, available - sidebar, (std::max)(1, height - title_height - inset)};
}

}  // namespace yukinal::desktop_host

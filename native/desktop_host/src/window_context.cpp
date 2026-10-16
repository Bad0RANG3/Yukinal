#include "yukinal/desktop_host/window_context.hpp"

#include <windows.h>

namespace yukinal::desktop_host {

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
    if (!name.is_string()) return std::nullopt;
    const auto utf8 = name.get<std::string>();
    if (utf8.size() > 640 || utf8.find('\0') != std::string::npos) return std::nullopt;
    const int length = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, utf8.data(),
        static_cast<int>(utf8.size()), nullptr, 0);
    if (length > 160 || (length == 0 && !utf8.empty())) return std::nullopt;
    std::wstring wide_name(static_cast<size_t>(length), L'\0');
    if (length > 0) MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, utf8.data(),
        static_cast<int>(utf8.size()), wide_name.data(), length);
    for (const wchar_t ch : wide_name) if (ch < 32 || ch == 127) return std::nullopt;
    if (section == "servers" && !wide_name.empty()) label += L"  /  " + wide_name;
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

}  // namespace yukinal::desktop_host

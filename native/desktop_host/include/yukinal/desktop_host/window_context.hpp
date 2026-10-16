#pragma once

#include <nlohmann/json.hpp>
#include <optional>
#include <string>
#include <vector>

namespace yukinal::desktop_host {

// Presentation only. This parser cannot dispatch application commands.
std::optional<std::wstring> window_context_label(const nlohmann::json& params);

struct WindowSidebarServer {
  std::string id;
  std::wstring name;
  std::wstring detail;
  std::string status;
};
struct WindowSidebar {
  std::optional<std::string> selected_server_id;
  std::vector<WindowSidebarServer> servers;
  bool loading = false;
  bool failed = false;
  bool blocked = false;
};
std::optional<WindowSidebar> parse_window_sidebar(const nlohmann::json& params);
struct WindowLayout {
  int sidebar_width;
  int content_x;
  int content_y;
  int content_width;
  int content_height;
};
WindowLayout window_layout(int width, int height, int title_height, int inset, int sidebar_width);

}  // namespace yukinal::desktop_host

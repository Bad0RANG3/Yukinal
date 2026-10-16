#pragma once
#include <webview/webview.h>
#include "yukinal/desktop_host/ipc_router.hpp"

namespace yukinal::desktop_host {
bool install_fused_frame(webview_t view);
void uninstall_fused_frame(webview_t view);
IpcReply update_shell_context(webview_t view, const nlohmann::json& params);
IpcReply update_shell_sidebar(webview_t view, const nlohmann::json& params);
}  // namespace yukinal::desktop_host

#include <nlohmann/json.hpp>
#include <httplib.h>
#include <webview/webview.h>

#include <atomic>
#include <filesystem>
#include <functional>
#include <mutex>
#include <iostream>
#include <memory>
#include <optional>
#include <regex>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#ifdef _WIN32
#include <shellapi.h>
#include <windows.h>
#endif

#include "yukinal/desktop_host/ipc_router.hpp"
#include "yukinal/desktop_host/database.hpp"
#ifdef _WIN32
#include "yukinal/desktop_host/windows_window_frame.hpp"
#endif

namespace {

struct Options {
  std::optional<std::string> dev_url;
  std::optional<std::filesystem::path> ui_directory;
  std::optional<std::filesystem::path> data_directory;
  bool self_check = false;
  bool help = false;
};

struct WindowEventLifetime {
  std::mutex mutex;
  std::atomic_bool alive{true};
};

struct BridgeContext {
  webview_t view = nullptr;
  std::shared_ptr<WindowEventLifetime> lifetime;
  std::shared_ptr<std::atomic_bool> window_alive = std::make_shared<std::atomic_bool>(true);
  std::shared_ptr<yukinal::desktop_host::IpcRouter> shared_router;
  yukinal::desktop_host::IpcRouter& router;
  std::function<yukinal::desktop_host::IpcReply(const nlohmann::json&)> open_workpage;
#ifdef _WIN32
  WNDPROC previous_proc = nullptr;
  bool auxiliary = false;
  bool pinned = false;
  std::string workpage_key;
#endif

  explicit BridgeContext(yukinal::desktop_host::DesktopDatabase& database)
      : lifetime(std::make_shared<WindowEventLifetime>()),
        shared_router(std::make_shared<yukinal::desktop_host::IpcRouter>(&database)), router(*shared_router) {}
  BridgeContext(std::shared_ptr<yukinal::desktop_host::IpcRouter> shared,
                std::shared_ptr<WindowEventLifetime> window_lifetime)
      : lifetime(std::move(window_lifetime)), shared_router(std::move(shared)), router(*shared_router) {}

  bool active() const { return lifetime->alive.load() && window_alive->load(); }

  void publish_event(const std::string& event, const nlohmann::json& payload) {
    if (!active() || router.subscribers(event).empty()) return;
    const auto envelope = nlohmann::json{{"event", event}, {"payload", payload}}.dump();
    const auto script = "(()=>{const e=JSON.parse(" + nlohmann::json(envelope).dump() +
                        ");window.__YUKINAL_HOST__?.dispatchEvent(e.event,e.payload)})();";
    struct PendingEvent {
      std::string script;
      std::shared_ptr<WindowEventLifetime> lifetime;
      std::shared_ptr<std::atomic_bool> window_alive;
    };
    auto* pending = new PendingEvent{script, lifetime, window_alive};
    const auto dispatched = webview_dispatch(
        view,
        [](webview_t current, void* value) {
          std::unique_ptr<PendingEvent> queued(static_cast<PendingEvent*>(value));
          // WebView destruction drains queued callbacks after releasing its browser.
          // Keep the gate alive independently of the BridgeContext and skip eval.
          if (!queued->lifetime->alive.load() || !queued->window_alive->load()) return;
          webview_eval(current, queued->script.c_str());
        },
        pending);
    if (dispatched != WEBVIEW_ERROR_OK) delete pending;
  }
};

void destroy_bridge_window(BridgeContext& context) {
  context.window_alive->store(false);
  if (context.view) webview_destroy(context.view);
  context.view = nullptr;
}

std::vector<std::unique_ptr<BridgeContext>> retire_auxiliary_windows(
    BridgeContext& context, std::vector<std::unique_ptr<BridgeContext>>& auxiliary_windows) {
  context.lifetime->alive.store(false);
  context.router.set_event_sink({});
  std::lock_guard lock(context.lifetime->mutex);
  std::vector<std::unique_ptr<BridgeContext>> retired;
  retired.swap(auxiliary_windows);
  for (const auto& child : retired) child->window_alive->store(false);
  return retired;
}

#ifdef _WIN32
LRESULT CALLBACK workpage_window_proc(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
  auto* context = reinterpret_cast<BridgeContext*>(GetPropW(window, L"YukinalBridgeContext"));
  if (!context || !context->previous_proc) return DefWindowProcW(window, message, wparam, lparam);
  if (message == WM_CLOSE) {
    if (context->auxiliary) {
      // Closing a view preserves its terminal and running operations until app exit.
      ShowWindow(window, SW_HIDE);
      return 0;
    }
    context->lifetime->alive.store(false);
    webview_terminate(context->view);
  }
  return CallWindowProcW(context->previous_proc, window, message, wparam, lparam);
}
void install_workpage_lifecycle(BridgeContext& context) {
  const auto window = static_cast<HWND>(webview_get_window(context.view));
  SetPropW(window, L"YukinalBridgeContext", &context);
  context.previous_proc = reinterpret_cast<WNDPROC>(SetWindowLongPtrW(window, GWLP_WNDPROC,
      reinterpret_cast<LONG_PTR>(workpage_window_proc)));
}
void uninstall_workpage_lifecycle(BridgeContext& context) {
  const auto window = static_cast<HWND>(webview_get_window(context.view));
  if (window && IsWindow(window)) {
    if (context.previous_proc) SetWindowLongPtrW(window, GWLP_WNDPROC, reinterpret_cast<LONG_PTR>(context.previous_proc));
    RemovePropW(window, L"YukinalBridgeContext");
  }
  context.previous_proc = nullptr;
}
#endif

class UiServer {
 public:
  UiServer() = default;
  UiServer(const UiServer&) = delete;
  UiServer& operator=(const UiServer&) = delete;

  ~UiServer() {
    server_.stop();
    if (thread_.joinable()) thread_.join();
  }

  bool start(const std::filesystem::path& root) {
    const auto root_bytes = root.generic_u8string();
    const std::string root_utf8(root_bytes.begin(), root_bytes.end());
    if (!server_.set_mount_point("/", root_utf8)) return false;

    server_.set_default_headers({
        {"X-Content-Type-Options", "nosniff"},
        {"Referrer-Policy", "no-referrer"},
        {"Cross-Origin-Resource-Policy", "same-origin"},
        {"Content-Security-Policy",
         "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; "
         "img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self'; "
         "object-src 'none'; base-uri 'self'; frame-ancestors 'none'"},
    });

    server_.set_pre_routing_handler([this](const httplib::Request& request,
                                           httplib::Response& response) {
      const auto expected_host = "127.0.0.1:" + std::to_string(port_);
      const auto origin = request.get_header_value("Origin");
      const auto expected_origin = "http://" + expected_host;
      if (request.get_header_value("Host") != expected_host ||
          (!origin.empty() && origin != expected_origin)) {
        response.status = 403;
        return httplib::Server::HandlerResponse::Handled;
      }
      if (request.method != "GET" && request.method != "HEAD") {
        response.status = 405;
        response.set_header("Allow", "GET, HEAD");
        return httplib::Server::HandlerResponse::Handled;
      }
      return httplib::Server::HandlerResponse::Unhandled;
    });

    port_ = server_.bind_to_any_port("127.0.0.1");
    if (port_ <= 0) return false;
    thread_ = std::thread([this] { (void)server_.listen_after_bind(); });
    server_.wait_until_ready();
    return true;
  }

  std::string url() const {
    return "http://127.0.0.1:" + std::to_string(port_) + "/";
  }

 private:
  httplib::Server server_;
  std::thread thread_;
  int port_ = 0;
};

std::optional<Options> parse_options(const std::vector<std::string>& arguments) {
  Options options;
  for (size_t index = 1; index < arguments.size(); ++index) {
    if (arguments[index] == "--dev-url" && index + 1 < arguments.size()) {
      options.dev_url = arguments[++index];
    } else if (arguments[index] == "--ui-dir" && index + 1 < arguments.size()) {
      options.ui_directory = std::filesystem::u8path(arguments[++index]);
    } else if (arguments[index] == "--data-dir" && index + 1 < arguments.size()) {
      options.data_directory = std::filesystem::u8path(arguments[++index]);
    } else if (arguments[index] == "--self-check") {
      options.self_check = true;
    } else if (arguments[index] == "--help") {
      options.help = true;
    } else {
      return std::nullopt;
    }
  }
  return options;
}

bool valid_dev_url(const std::string& value) {
  static const std::regex pattern(R"(^http://127\.0\.0\.1:[1-9][0-9]{0,4}/?$)");
  if (!std::regex_match(value, pattern)) return false;
  const auto colon = value.rfind(':');
  try {
    const auto port = std::stoi(value.substr(colon + 1));
    return port <= 65535;
  } catch (...) {
    return false;
  }
}

std::optional<std::string> resolve_ui_url(const Options& options, UiServer& ui_server) {
  if (options.dev_url.has_value() == options.ui_directory.has_value()) return std::nullopt;
  if (options.dev_url) {
    if (!valid_dev_url(*options.dev_url)) return std::nullopt;
    return *options.dev_url;
  }
  std::error_code error;
  const auto root = std::filesystem::weakly_canonical(*options.ui_directory, error);
  if (error || !std::filesystem::is_directory(root, error) || error) return std::nullopt;
  const auto index = root / "index.html";
  if (!std::filesystem::is_regular_file(index, error) || error) return std::nullopt;
  if (!ui_server.start(root)) return std::nullopt;
  return ui_server.url();
}

void rpc_handler(const char* id, const char* request, void* context_pointer) {
  auto& context = *static_cast<BridgeContext*>(context_pointer);
  // Queued native callbacks can run while webview_destroy drains its event queue.
  // There is no browser left to receive a rejection, so do not touch the view.
  if (!context.active()) return;
  nlohmann::json reply;
  bool ok = false;
  try {
    if (request == nullptr || std::char_traits<char>::length(request) > 24 * 1024 * 1024) {
      reply = {{"code", "INVALID_REQUEST"}, {"message", "Desktop IPC request is invalid"}};
    } else {
      auto arguments = nlohmann::json::parse(request);
      if (!arguments.is_array() || arguments.size() != 2 || !arguments[0].is_string()) {
        reply = {{"code", "INVALID_REQUEST"}, {"message", "Desktop IPC request is invalid"}};
      } else {
        const auto method = arguments[0].get<std::string>();
#ifdef _WIN32
        if (method == "__shell.pin" && context.auxiliary) {
          const auto& params = arguments[1];
          if (!params.is_object() || params.size() != 1 || !params.contains("pinned") || !params["pinned"].is_boolean()) {
            webview_return(context.view, id, 1, R"({"code":"INVALID_PARAMS","message":"Pin requires a boolean"})");
            return;
          }
          const bool pinned = params["pinned"].get<bool>();
          const bool changed = SetWindowPos(static_cast<HWND>(webview_get_window(context.view)),
              pinned ? HWND_TOPMOST : HWND_NOTOPMOST, 0, 0, 0, 0,
              SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE) != FALSE;
          if (!changed) {
            webview_return(context.view, id, 1, R"({"code":"WINDOW_ERROR","message":"Could not change window pin state"})");
            return;
          }
          context.pinned = pinned;
          const auto value = nlohmann::json{{"pinned", pinned}}.dump();
          webview_return(context.view, id, 0, value.c_str());
          return;
        }
        const auto result = method == "__shell.open_workpage" && context.open_workpage
            ? context.open_workpage(arguments[1])
            : method == "__shell.hide" && context.auxiliary
            ? (ShowWindow(static_cast<HWND>(webview_get_window(context.view)), SW_HIDE),
                yukinal::desktop_host::IpcReply{true, {{"hidden", true}}})
            : method == "__shell.update_context"
            ? yukinal::desktop_host::update_shell_context(context.view, arguments[1])
            : method == "__shell.update_surface"
            ? yukinal::desktop_host::update_caption_surface(context.view, arguments[1])
            : context.router.invoke(method, std::move(arguments[1]));
#else
        const auto result = context.router.invoke(method, std::move(arguments[1]));
#endif
        ok = result.ok;
        reply = result.value;
      }
    }
  } catch (const nlohmann::json::exception&) {
    reply = {{"code", "INVALID_REQUEST"}, {"message", "Desktop IPC request is invalid"}};
  } catch (...) {
    reply = {{"code", "INTERNAL_ERROR"}, {"message", "Desktop IPC request failed"}};
  }
  if (!context.active()) return;
  const auto serialized = reply.dump();
  webview_return(context.view, id, ok ? 0 : 1, serialized.c_str());
}

constexpr const char* kHostBridge = R"JS(
(() => {
  if (window.top !== window) return;
  const subscriptions = new Map();
  const nativeCall = (method, params) => window.yukinalNativeInvoke(method, params);
  const check = (result) => {
    if (result && typeof result.code === "string" && typeof result.message === "string") {
      throw new Error(`${result.message} (${result.code})`);
    }
    return result;
  };
  const normalizeError = (error, fallback) => {
    if (error instanceof Error) return error;
    if (error && typeof error.code === "string" && typeof error.message === "string") {
      return new Error(`${error.message} (${error.code})`);
    }
    return new Error(fallback);
  };
  window.__YUKINAL_HOST__ = {
    invoke: (command, params = {}) => nativeCall(command, params).then(check).catch((error) => {
      throw normalizeError(error, "The C++ desktop host rejected the request");
    }),
    listen: (event, handler) => {
      const token = crypto.randomUUID();
      let handlers = subscriptions.get(event);
      if (!handlers) subscriptions.set(event, handlers = new Map());
      handlers.set(token, handler);
      return nativeCall("__events.subscribe", { event, token }).then(() => {
        let stopped = false;
        return () => {
          if (stopped) return;
          stopped = true;
          handlers.delete(token);
          if (handlers.size === 0) subscriptions.delete(event);
          void nativeCall("__events.unsubscribe", { event, token });
        };
      }).catch((error) => {
        handlers.delete(token);
        if (handlers.size === 0) subscriptions.delete(event);
        throw error;
      });
    },
    openUrl: (url) => nativeCall("shell.open_url", { url }).then(check).catch((error) => {
      throw normalizeError(error, "The C++ desktop host could not open the URL");
    }),
    dispatchEvent: (event, payload) => {
      for (const handler of subscriptions.get(event)?.values() ?? []) {
        try { handler(payload); } catch (error) { console.error("desktop event handler failed", error); }
      }
    },
  };
})();
)JS";

int run(const std::vector<std::string>& arguments) {
  const auto options = parse_options(arguments);
  if (!options) return 2;
  if (options->help) return 0;
  if (options->self_check) {
    yukinal::desktop_host::IpcRouter router;
    const auto ping = router.invoke("core_ping", nlohmann::json::object());
    return ping.ok && ping.value.value("version", "") == YUKINAL_DESKTOP_VERSION ? 0 : 1;
  }
  UiServer ui_server;
  const auto url = resolve_ui_url(*options, ui_server);
  if (!url) return 2;

  std::unique_ptr<yukinal::desktop_host::DesktopDatabase> database;
  try {
    database = std::make_unique<yukinal::desktop_host::DesktopDatabase>(
        options->data_directory.value_or(yukinal::desktop_host::default_data_directory()));
  } catch (const std::exception&) {
    return 4;
  }

  try {
    const bool debug = options->dev_url.has_value();
    BridgeContext context(*database);
    std::vector<std::unique_ptr<BridgeContext>> auxiliary_windows;
    auto window_events = context.lifetime;
    context.view = webview_create(debug ? 1 : 0, nullptr);
    if (context.view == nullptr) return 3;
    context.router.set_event_sink(
        [&context, &auxiliary_windows, window_events](const std::string& event, const nlohmann::json& payload) {
          std::lock_guard lock(window_events->mutex);
          if (!window_events->alive.load()) return;
          context.publish_event(event, payload);
          for (const auto& child : auxiliary_windows) child->publish_event(event, payload);
        });
#ifdef _WIN32
    const bool titlebar_overlay = yukinal::desktop_host::install_fused_frame(context.view);
    const std::string shell_bridge = std::string("window.__YUKINAL_HOST__.windowShell={titlebarOverlay:") +
        (titlebar_overlay ? "true" : "false") + ",updateContext:(context)=>"
        "window.__YUKINAL_HOST__.invoke('__shell.update_context',context).then(()=>undefined),"
        "updateSurface:(surface)=>window.__YUKINAL_HOST__.invoke('__shell.update_surface',surface).then(()=>undefined),"
        "openWorkpage:(page)=>window.__YUKINAL_HOST__.invoke('__shell.open_workpage',page)};";
    context.open_workpage = [&](const nlohmann::json& page) -> yukinal::desktop_host::IpcReply {
      using yukinal::desktop_host::IpcReply;
      if (!context.active()) return {false, {{"code", "SHUTTING_DOWN"}, {"message", "The app is closing"}}};
      if (!page.is_object() || page.size() != 2 || !page.contains("kind") || !page["kind"].is_string() ||
          !page.contains("serverId") || !page["serverId"].is_string()) {
        return {false, {{"code", "INVALID_PARAMS"}, {"message", "A workpage requires a kind and server"}}};
      }
      const auto kind = page["kind"].get<std::string>();
      const auto server_id = page["serverId"].get<std::string>();
      if ((kind != "files" && kind != "terminal" && kind != "logs" && kind != "services") ||
          server_id.empty() || server_id.size() > 256 ||
          !std::regex_match(server_id, std::regex("[A-Za-z0-9_-]+"))) {
        return {false, {{"code", "INVALID_PARAMS"}, {"message", "This workpage cannot be detached"}}};
      }
      const auto servers = database->list_servers().at("servers");
      bool found = false;
      for (const auto& server : servers) if (server.at("id") == server_id) found = true;
      if (!found) return {false, {{"code", "NOT_FOUND"}, {"message", "Server no longer exists"}}};
      const auto key = kind + ":" + server_id;
      {
        std::lock_guard lock(window_events->mutex);
        for (const auto& child : auxiliary_windows) if (child->workpage_key == key) {
          const auto window = static_cast<HWND>(webview_get_window(child->view));
          ShowWindow(window, SW_RESTORE); SetForegroundWindow(window);
          return {true, {{"opened", true}}};
        }
        if (auxiliary_windows.size() >= 12) return {false, {{"code", "WINDOW_LIMIT"}, {"message", "Close the app to release inactive workpage windows"}}};
      }
      auto child = std::make_unique<BridgeContext>(context.shared_router, window_events);
      child->auxiliary = true; child->workpage_key = key;
      child->view = webview_create(debug ? 1 : 0, nullptr);
      if (!child->view) return {false, {{"code", "WINDOW_ERROR"}, {"message", "Could not create a workpage window"}}};
      if (!context.active()) {
        destroy_bridge_window(*child);
        return {false, {{"code", "SHUTTING_DOWN"}, {"message", "The app is closing"}}};
      }
      webview_set_title(child->view, ("Yukinal · " + kind).c_str());
      webview_set_size(child->view, 480, 360, WEBVIEW_HINT_MIN);
      webview_set_size(child->view, 940, 640, WEBVIEW_HINT_NONE);
      if (webview_bind(child->view, "yukinalNativeInvoke", rpc_handler, child.get()) != WEBVIEW_ERROR_OK ||
          webview_init(child->view, kHostBridge) != WEBVIEW_ERROR_OK ||
          webview_init(child->view, "window.__YUKINAL_HOST__.workpageWindow={setPinned:(pinned)=>window.__YUKINAL_HOST__.invoke('__shell.pin',{pinned}),hide:()=>window.__YUKINAL_HOST__.invoke('__shell.hide',{})};") != WEBVIEW_ERROR_OK) {
        destroy_bridge_window(*child);
        return {false, {{"code", "WINDOW_ERROR"}, {"message", "Could not initialize the workpage bridge"}}};
      }
      if (!context.active()) {
        destroy_bridge_window(*child);
        return {false, {{"code", "SHUTTING_DOWN"}, {"message", "The app is closing"}}};
      }
      install_workpage_lifecycle(*child);
      const auto child_url = *url + "?workpage=" + kind + "&server=" + server_id;
      if (webview_navigate(child->view, child_url.c_str()) != WEBVIEW_ERROR_OK) {
        uninstall_workpage_lifecycle(*child);
        destroy_bridge_window(*child);
        return {false, {{"code", "WINDOW_ERROR"}, {"message", "Could not load the workpage"}}};
      }
      { std::lock_guard lock(window_events->mutex); auxiliary_windows.push_back(std::move(child)); }
      return {true, {{"opened", true}}};
    };
#endif
    if (webview_set_title(context.view, "Yukinal") != WEBVIEW_ERROR_OK ||
#ifdef _WIN32
        webview_set_size(context.view, 1100, 680, WEBVIEW_HINT_MIN) != WEBVIEW_ERROR_OK ||
#endif
        webview_set_size(context.view, 1440, 900, WEBVIEW_HINT_NONE) != WEBVIEW_ERROR_OK ||
        webview_bind(context.view, "yukinalNativeInvoke", rpc_handler, &context) !=
            WEBVIEW_ERROR_OK ||
        webview_init(context.view, kHostBridge) != WEBVIEW_ERROR_OK ||
#ifdef _WIN32
        webview_init(context.view, shell_bridge.c_str()) != WEBVIEW_ERROR_OK ||
#endif
        webview_navigate(context.view, url->c_str()) != WEBVIEW_ERROR_OK) {
#ifdef _WIN32
      yukinal::desktop_host::uninstall_fused_frame(context.view);
#endif
      auto retired = retire_auxiliary_windows(context, auxiliary_windows);
      for (auto& child : retired) {
#ifdef _WIN32
        uninstall_workpage_lifecycle(*child);
#endif
        destroy_bridge_window(*child);
      }
      destroy_bridge_window(context);
      return 3;
    }
    #ifdef _WIN32
    install_workpage_lifecycle(context);
    #endif
    const auto result = webview_run(context.view);
    auto retired = retire_auxiliary_windows(context, auxiliary_windows);
    // Keep all contexts alive while destruction pumps callbacks from any window.
    // Never hold the event mutex across that reentrant drain.
    for (auto& child : retired) {
        #ifdef _WIN32
        uninstall_workpage_lifecycle(*child);
        #endif
      destroy_bridge_window(*child);
    }
#ifdef _WIN32
    uninstall_workpage_lifecycle(context);
    yukinal::desktop_host::uninstall_fused_frame(context.view);
#endif
    destroy_bridge_window(context);
    return result == WEBVIEW_ERROR_OK ? 0 : 3;
  } catch (const webview::exception&) {
    return 3;
  }
}

}  // namespace

#ifdef _WIN32
namespace {
std::string to_utf8(const wchar_t* value) {
  const int length = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value, -1,
                                        nullptr, 0, nullptr, nullptr);
  if (length <= 1) return {};
  std::string converted(static_cast<size_t>(length), '\0');
  if (WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value, -1, converted.data(), length,
                          nullptr, nullptr) <= 0) {
    return {};
  }
  converted.pop_back();
  return converted;
}
}  // namespace

int WINAPI wWinMain(HINSTANCE, HINSTANCE, PWSTR, int) {
  int count = 0;
  auto** wide_arguments = CommandLineToArgvW(GetCommandLineW(), &count);
  if (wide_arguments == nullptr) return 2;
  std::vector<std::string> arguments;
  arguments.reserve(static_cast<size_t>(count));
  for (int index = 0; index < count; ++index) arguments.push_back(to_utf8(wide_arguments[index]));
  LocalFree(wide_arguments);
  return run(arguments);
}
#else
int main(int argc, char** argv) {
  return run(std::vector<std::string>(argv, argv + argc));
}
#endif

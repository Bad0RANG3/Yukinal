#include <nlohmann/json.hpp>
#include <httplib.h>
#include <webview/webview.h>

#include <filesystem>
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

struct BridgeContext {
  webview_t view = nullptr;
  yukinal::desktop_host::IpcRouter router;

  explicit BridgeContext(yukinal::desktop_host::DesktopDatabase& database)
      : router(&database) {}

  void publish_event(const std::string& event, const nlohmann::json& payload) {
    if (router.subscribers(event).empty()) return;
    const auto envelope = nlohmann::json{{"event", event}, {"payload", payload}}.dump();
    const auto script = "window.__YUKINAL_HOST__?.dispatchEvent(JSON.parse(" +
                        nlohmann::json(envelope).dump() + "));";
    auto* pending = new std::string(script);
    const auto dispatched = webview_dispatch(
        view,
        [](webview_t current, void* value) {
          std::unique_ptr<std::string> queued(static_cast<std::string*>(value));
          webview_eval(current, queued->c_str());
        },
        pending);
    if (dispatched != WEBVIEW_ERROR_OK) delete pending;
  }
};

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
        const auto result = method == "__shell.update_context"
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
  const auto serialized = reply.dump();
  webview_return(context.view, id, ok ? 0 : 1, serialized.c_str());
}

constexpr const char* kHostBridge = R"JS(
(() => {
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
    context.view = webview_create(debug ? 1 : 0, nullptr);
    if (context.view == nullptr) return 3;
#ifdef _WIN32
    const bool titlebar_overlay = yukinal::desktop_host::install_fused_frame(context.view);
    const std::string shell_bridge = std::string("window.__YUKINAL_HOST__.windowShell={titlebarOverlay:") +
        (titlebar_overlay ? "true" : "false") + ",updateContext:(context)=>"
        "window.__YUKINAL_HOST__.invoke('__shell.update_context',context).then(()=>undefined),"
        "updateSurface:(surface)=>window.__YUKINAL_HOST__.invoke('__shell.update_surface',surface).then(()=>undefined)};";
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
      webview_destroy(context.view);
      return 3;
    }
    const auto result = webview_run(context.view);
#ifdef _WIN32
    yukinal::desktop_host::uninstall_fused_frame(context.view);
#endif
    webview_destroy(context.view);
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

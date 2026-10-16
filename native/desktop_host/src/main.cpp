#include <nlohmann/json.hpp>
#include <httplib.h>
#include <webview/webview.h>

#ifdef _WIN32
#include <dwmapi.h>
#include "yukinal/desktop_host/window_context.hpp"

#ifndef DWMWA_WINDOW_CORNER_PREFERENCE
#define DWMWA_WINDOW_CORNER_PREFERENCE 33
#endif
#ifndef DWMWCP_ROUND
#define DWMWCP_ROUND 2
#endif
#ifndef DWMWA_CAPTION_COLOR
#define DWMWA_CAPTION_COLOR 35
#endif
#endif

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

#ifdef _WIN32
constexpr COLORREF kOuterColor = RGB(0x25, 0x25, 0x25);
constexpr wchar_t kFrameProperty[] = L"YukinalFrame";
constexpr int kChromeHeight = 40;
constexpr int kContentInset = 8;
constexpr int kContentRadius = 14;

struct FrameState {
  HWND widget = nullptr;
  WNDPROC previous = nullptr;
  bool reshaping = false;
  std::wstring context_label;
};

FrameState* frame_state(HWND window) {
  return static_cast<FrameState*>(GetPropW(window, kFrameProperty));
}

UINT frame_dpi(HWND window) {
  auto get_dpi = reinterpret_cast<UINT(WINAPI*)(HWND)>(
      GetProcAddress(GetModuleHandleW(L"user32.dll"), "GetDpiForWindow"));
  const UINT dpi = get_dpi == nullptr ? 96 : get_dpi(window);
  return dpi == 0 ? 96 : dpi;
}

int frame_scale(HWND window, int value) {
  return MulDiv(value, static_cast<int>(frame_dpi(window)), 96);
}

int frame_metric(HWND window, int metric) {
  auto for_dpi = reinterpret_cast<int(WINAPI*)(int, UINT)>(
      GetProcAddress(GetModuleHandleW(L"user32.dll"), "GetSystemMetricsForDpi"));
  return for_dpi == nullptr ? GetSystemMetrics(metric) : for_dpi(metric, frame_dpi(window));
}

void layout_native_content(HWND window, FrameState& state) {
  if (state.reshaping || !IsWindow(state.widget) || IsIconic(window)) return;
  state.reshaping = true;
  RECT client{};
  GetClientRect(window, &client);
  const int inset = frame_scale(window, kContentInset);
  const int top = frame_scale(window, kChromeHeight);
  const int width = (client.right - 2 * inset) > 0 ? client.right - 2 * inset : 1;
  const int height = (client.bottom - top - inset) > 0 ? client.bottom - top - inset : 1;
  // Moving the actual WebView child also updates the controller bounds through
  // its WM_SIZE handler. These are window coordinates, never CSS padding.
  MoveWindow(state.widget, inset, top, width, height, TRUE);
  const int diameter = frame_scale(window, 2 * kContentRadius);
  const HRGN region = CreateRoundRectRgn(0, 0, width + 1, height + 1, diameter, diameter);
  if (region != nullptr && !SetWindowRgn(state.widget, region, TRUE)) DeleteObject(region);
  InvalidateRect(window, nullptr, FALSE);
  state.reshaping = false;
}

void paint_native_chrome(HWND window) {
  PAINTSTRUCT paint{};
  const HDC window_dc = BeginPaint(window, &paint);
  RECT client{};
  GetClientRect(window, &client);
  const HBRUSH background = CreateSolidBrush(kOuterColor);
  FillRect(window_dc, &client, background);
  // The inset border belongs to the native child window, like its clipping.
  const int inset = frame_scale(window, kContentInset);
  const int top = frame_scale(window, kChromeHeight);
  const HPEN border = CreatePen(PS_SOLID, 1, RGB(54, 54, 56));
  const HGDIOBJ previous_pen = SelectObject(window_dc, border);
  const HGDIOBJ previous_brush = SelectObject(window_dc, GetStockObject(HOLLOW_BRUSH));
  RoundRect(window_dc, inset - 1, top - 1, client.right - inset + 1,
      client.bottom - inset + 1, frame_scale(window, kContentRadius * 2),
      frame_scale(window, kContentRadius * 2));
  SelectObject(window_dc, previous_brush);
  SelectObject(window_dc, previous_pen);
  DeleteObject(border);
  // GDI's default alpha is zero. On an extended DWM caption that produces a
  // washed-out system tint. Composite an opaque 32-bit strip so the caption
  // matches the application surface while DWM still draws its native controls.
  BITMAPINFO info{};
  info.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
  info.bmiHeader.biWidth = client.right;
  info.bmiHeader.biHeight = -top;
  info.bmiHeader.biPlanes = 1;
  info.bmiHeader.biBitCount = 32;
  info.bmiHeader.biCompression = BI_RGB;
  void* pixels = nullptr;
  const HBITMAP bitmap = CreateDIBSection(window_dc, &info, DIB_RGB_COLORS, &pixels, nullptr, 0);
  const HDC dc = CreateCompatibleDC(window_dc);
  if (bitmap == nullptr || dc == nullptr || pixels == nullptr) {
    if (dc != nullptr) DeleteDC(dc);
    if (bitmap != nullptr) DeleteObject(bitmap);
    DeleteObject(background);
    EndPaint(window, &paint);
    return;
  }
  const HGDIOBJ previous_bitmap = SelectObject(dc, bitmap);
  const RECT caption{0, 0, client.right, top};
  FillRect(dc, &caption, background);
  DeleteObject(background);
  SetBkMode(dc, TRANSPARENT);
  const bool active = GetActiveWindow() == window;
  SetTextColor(dc, active ? RGB(220, 220, 224) : RGB(150, 150, 156));
  const HFONT font = CreateFontW(-frame_scale(window, 12), 0, 0, 0, FW_SEMIBOLD,
      FALSE, FALSE, FALSE, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
      CLEARTYPE_QUALITY, DEFAULT_PITCH, L"Segoe UI");
  const HGDIOBJ previous_font = SelectObject(dc, font);
  // Reserve the system caption-button strip, including at high DPI.
  RECT brand{frame_scale(window, 20), 0, frame_scale(window, 75), top};
  DrawTextW(dc, L"Yukinal", -1, &brand, DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
  const HFONT context_font = CreateFontW(-frame_scale(window, 12), 0, 0, 0, FW_NORMAL,
      FALSE, FALSE, FALSE, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
      CLEARTYPE_QUALITY, DEFAULT_PITCH, L"Segoe UI");
  SelectObject(dc, context_font);
  SetTextColor(dc, active ? RGB(161, 161, 169) : RGB(128, 128, 136));
  RECT divider{frame_scale(window, 78), 0, frame_scale(window, 88), top};
  DrawTextW(dc, L"/", -1, &divider, DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
  RECT title{frame_scale(window, 96), 0,
             client.right - frame_scale(window, 160), top};
  const auto* state = frame_state(window);
  DrawTextW(dc, state ? state->context_label.c_str() : L"", -1, &title,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
  SelectObject(dc, previous_font);
  DeleteObject(context_font);
  DeleteObject(font);
  GdiFlush();
  auto* bytes = static_cast<unsigned char*>(pixels);
  const int caption_start = (std::max)(0, static_cast<int>(client.right) - frame_scale(window, 160));
  for (int row = 0; row < top; ++row) {
    for (int column = 0; column < client.right; ++column) {
      const size_t pixel = (static_cast<size_t>(row) * client.right + column) * 4;
      if (column < caption_start) {
        bytes[pixel + 3] = 255;
      } else {
        // Leave DWM's caption-button strip transparent, including its hover
        // visuals. Painting it opaque would cover the system controls.
        bytes[pixel] = bytes[pixel + 1] = bytes[pixel + 2] = bytes[pixel + 3] = 0;
      }
    }
  }
  BitBlt(window_dc, 0, 0, client.right, top, dc, 0, 0, SRCCOPY);
  SelectObject(dc, previous_bitmap);
  DeleteDC(dc);
  DeleteObject(bitmap);
  EndPaint(window, &paint);
}

LRESULT CALLBACK fused_frame_proc(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
  auto* state = frame_state(window);
  if (state == nullptr || state->previous == nullptr) return DefWindowProcW(window, message, wparam, lparam);

  // Keep Windows caption buttons and Snap Layouts, rather than HTML replicas.
  LRESULT dwm_result = 0;
  const bool dwm_handled = DwmDefWindowProc(window, message, wparam, lparam, &dwm_result) != FALSE;
  if (message == WM_NCHITTEST) {
    if (dwm_handled) return dwm_result;
    const auto standard = CallWindowProcW(state->previous, window, message, wparam, lparam);
    if (standard != HTCLIENT) return standard;
    RECT bounds{};
    GetWindowRect(window, &bounds);
    const int x = static_cast<short>(LOWORD(lparam));
    const int y = static_cast<short>(HIWORD(lparam));
    const int edge = frame_metric(window, SM_CYFRAME) + frame_metric(window, SM_CXPADDEDBORDER);
    if (!IsZoomed(window) && y < bounds.top + edge) return HTTOP;
    POINT point{x, y};
    ScreenToClient(window, &point);
    // Leave the native caption-button strip to DefWindowProc even if DWM
    // reports no hit; it must never become part of the draggable title.
    if (point.x >= bounds.right - bounds.left - frame_scale(window, 160)) return HTCLIENT;
    if (point.y < frame_scale(window, kChromeHeight)) return HTCAPTION;
    return HTCLIENT;
  }
  if (message == WM_NCCALCSIZE && wparam) {
    auto* params = reinterpret_cast<NCCALCSIZE_PARAMS*>(lparam);
    const int x = frame_metric(window, SM_CXFRAME) + frame_metric(window, SM_CXPADDEDBORDER);
    const int y = frame_metric(window, SM_CYFRAME) + frame_metric(window, SM_CXPADDEDBORDER);
    // Preserve native resize edges. Only the top caption joins our chrome.
    params->rgrc[0].left += x;
    params->rgrc[0].right -= x;
    params->rgrc[0].bottom -= y;
    if (IsZoomed(window)) params->rgrc[0].top += y;
    return 0;
  }
  if (message == WM_PAINT) { paint_native_chrome(window); return 0; }
  if (message == WM_ERASEBKGND) return 1;
  if (dwm_handled) return dwm_result;

  const auto result = CallWindowProcW(state->previous, window, message, wparam, lparam);
  if (message == WM_SIZE || message == WM_DPICHANGED) {
    layout_native_content(window, *state);
    if (message == WM_DPICHANGED) {
      const MARGINS glass{0, 0, frame_scale(window, kChromeHeight), 0};
      DwmExtendFrameIntoClientArea(window, &glass);
    }
  } else if (message == WM_ACTIVATE || message == WM_SETTINGCHANGE) {
    InvalidateRect(window, nullptr, FALSE);
  } else if (message == WM_NCDESTROY) {
    RemovePropW(window, kFrameProperty);
    delete state;
  }
  return result;
}

void install_fused_frame(webview_t view) {
  auto window = static_cast<HWND>(webview_get_window(view));
  auto widget = static_cast<HWND>(webview_get_native_handle(view, WEBVIEW_NATIVE_HANDLE_KIND_UI_WIDGET));
  if (window == nullptr || widget == nullptr || frame_state(window) != nullptr) return;
  auto state = std::make_unique<FrameState>();
  state->widget = widget;
  if (!SetPropW(window, kFrameProperty, state.get())) return;
  state->previous = reinterpret_cast<WNDPROC>(
      SetWindowLongPtrW(window, GWLP_WNDPROC, reinterpret_cast<LONG_PTR>(fused_frame_proc)));
  if (state->previous == nullptr) { RemovePropW(window, kFrameProperty); return; }
  auto* installed = state.release();
  // WS_CAPTION remains present so DWM owns minimize/maximize/close and Snap.
  const auto style = GetWindowLongPtrW(window, GWL_STYLE);
  SetWindowLongPtrW(window, GWL_STYLE, style | WS_CAPTION | WS_THICKFRAME |
                    WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_SYSMENU | WS_CLIPCHILDREN);
  // DWM needs the entire caption strip to render the native window controls.
  const MARGINS glass{0, 0, frame_scale(window, kChromeHeight), 0};
  DwmExtendFrameIntoClientArea(window, &glass);
  const int corners = DWMWCP_ROUND;
  DwmSetWindowAttribute(window, DWMWA_WINDOW_CORNER_PREFERENCE, &corners, sizeof(corners));
  DwmSetWindowAttribute(window, DWMWA_CAPTION_COLOR, &kOuterColor, sizeof(kOuterColor));
  const BOOL dark = TRUE;
  DwmSetWindowAttribute(window, 20 /* DWMWA_USE_IMMERSIVE_DARK_MODE */, &dark, sizeof(dark));
  SetWindowPos(window, nullptr, 0, 0, 0, 0,
               SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
  layout_native_content(window, *installed);
}

// This channel changes presentation only. Future native actions must route back
// to the existing application commands, never create a second execution path.
yukinal::desktop_host::IpcReply update_shell_context(webview_t view, const nlohmann::json& params) {
  const auto label = yukinal::desktop_host::window_context_label(params);
  if (!label) return {false, {{"code", "INVALID_REQUEST"}, {"message", "Window context is invalid"}}};
  const auto window = static_cast<HWND>(webview_get_window(view));
  auto* state = window == nullptr ? nullptr : frame_state(window);
  if (state == nullptr) return {false, {{"code", "UNAVAILABLE"}, {"message", "Native window shell is unavailable"}}};
  state->context_label = *label;
  const std::wstring window_title = L"Yukinal  |  " + *label;
  SetWindowTextW(window, window_title.c_str());
  InvalidateRect(window, nullptr, FALSE);
  return {true, nlohmann::json::object()};
}

void uninstall_fused_frame(webview_t view) {
  const auto window = static_cast<HWND>(webview_get_window(view));
  if (window == nullptr) return;
  if (auto* state = frame_state(window)) {
    SetWindowLongPtrW(window, GWLP_WNDPROC, reinterpret_cast<LONG_PTR>(state->previous));
    RemovePropW(window, kFrameProperty);
    delete state;
  }
}
#endif

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
            ? update_shell_context(context.view, arguments[1])
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
    install_fused_frame(context.view);
#endif
    if (webview_set_title(context.view, "Yukinal") != WEBVIEW_ERROR_OK ||
        webview_set_size(context.view, 1440, 900, WEBVIEW_HINT_NONE) != WEBVIEW_ERROR_OK ||
        webview_bind(context.view, "yukinalNativeInvoke", rpc_handler, &context) !=
            WEBVIEW_ERROR_OK ||
        webview_init(context.view, kHostBridge) != WEBVIEW_ERROR_OK ||
#ifdef _WIN32
        webview_init(context.view,
            "window.__YUKINAL_HOST__.windowShell={updateContext:(context)=>"
            "window.__YUKINAL_HOST__.invoke('__shell.update_context',context).then(()=>undefined)};") != WEBVIEW_ERROR_OK ||
#endif
        webview_navigate(context.view, url->c_str()) != WEBVIEW_ERROR_OK) {
#ifdef _WIN32
      uninstall_fused_frame(context.view);
#endif
      webview_destroy(context.view);
      return 3;
    }
    const auto result = webview_run(context.view);
#ifdef _WIN32
    uninstall_fused_frame(context.view);
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

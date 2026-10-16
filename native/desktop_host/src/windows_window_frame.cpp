#include "yukinal/desktop_host/windows_window_frame.hpp"
#include "yukinal/desktop_host/window_context.hpp"
#include "yukinal/desktop_host/caption_hit_test.hpp"

#include <windows.h>
#include <dwmapi.h>
#include <algorithm>
#include <cmath>
#include <memory>
#include <regex>

#ifndef DWMWA_WINDOW_CORNER_PREFERENCE
#define DWMWA_WINDOW_CORNER_PREFERENCE 33
#endif
#ifndef DWMWCP_ROUND
#define DWMWCP_ROUND 2
#endif
#ifndef DWMWA_CAPTION_COLOR
#define DWMWA_CAPTION_COLOR 35
#endif

namespace yukinal::desktop_host {

// Match the renderer's neutral outer surface after its monochrome filter.
constexpr COLORREF kOuterColor = RGB(0x1d, 0x1d, 0x1d);
constexpr wchar_t kFrameProperty[] = L"YukinalFrame";
constexpr int kChromeHeight = 40;

struct CaptionSurface {
  double viewport_width = 0;
  double left = 0;
  double right = 0;
  double radius = 0;
  double inspector_left = 0;
  COLORREF background = 0;
  COLORREF border = 0;
};

struct FrameState {
  HWND widget = nullptr;
  WNDPROC previous = nullptr;
  bool reshaping = false;
  bool titlebar_controls = false;
  std::optional<CaptionSurface> surface;
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

LRESULT caption_button_hit_test(HWND window, LPARAM position) {
  RECT bounds{}, buttons{};
  if (!GetWindowRect(window, &bounds) ||
      FAILED(DwmGetWindowAttribute(window, DWMWA_CAPTION_BUTTON_BOUNDS,
                                   &buttons, sizeof(buttons)))) return HTNOWHERE;
  using yukinal::desktop_host::CaptionButton;
  const auto button = yukinal::desktop_host::hit_test_caption_buttons(
      {buttons.left, buttons.top, buttons.right, buttons.bottom},
      bounds.left, bounds.top,
      static_cast<short>(LOWORD(position)), static_cast<short>(HIWORD(position)));
  switch (button) {
    case CaptionButton::minimize: return HTMINBUTTON;
    case CaptionButton::maximize: return HTMAXBUTTON;
    case CaptionButton::close: return HTCLOSE;
    default: return HTNOWHERE;
  }
}

// Only the central surface crosses the native caption. Its CSS geometry and
// colors come from the renderer, so resizing and content density cannot drift.
void paint_caption_surface(HWND window, const RECT& client, int height, unsigned char* pixels,
                           const CaptionSurface& surface) {
  const double scale = client.right / surface.viewport_width;
  const double left = surface.left * scale;
  const double right = surface.right * scale;
  const double radius = (std::min)(surface.radius * scale, (right - left) / 2);
  const double top = 12 * scale;
  const double edge = (std::max)(1.0, scale);
  const double shadow_width = (std::max)(1.0, 5 * scale);
  const int first = (std::max)(0, static_cast<int>(left - shadow_width * 3));
  const int last = (std::min)(static_cast<int>(client.right), static_cast<int>(right + shadow_width * 3));
  for (int row = 0; row < height; ++row) {
    for (int column = first; column < last; ++column) {
      const double x = column + 0.5;
      const double y = row + 0.5;
      double distance = (std::max)(left - x, x - right);
      if (y < top + radius) {
        if (x < left + radius) distance = std::hypot(x - left - radius, y - top - radius) - radius;
        else if (x > right - radius) distance = std::hypot(x - right + radius, y - top - radius) - radius;
        else distance = top - y;
      }
      const double coverage = std::clamp(0.5 - distance, 0.0, 1.0);
      const double interior = std::clamp(0.5 - distance - edge, 0.0, 1.0);
      const double shadow = 0.065 * std::exp(-(std::max)(0.0, distance) / shadow_width);
      auto* pixel = pixels + (static_cast<size_t>(row) * client.right + column) * 4;
      const unsigned char fill[]{GetBValue(surface.background), GetGValue(surface.background), GetRValue(surface.background)};
      const unsigned char border[]{GetBValue(surface.border), GetGValue(surface.border), GetRValue(surface.border)};
      for (int channel = 0; channel < 3; ++channel) {
        const double outer = pixel[channel] * (1 - shadow);
        const double color = outer * (1 - coverage) + border[channel] * (coverage - interior) + fill[channel] * interior;
        pixel[channel] = static_cast<unsigned char>(std::clamp(std::round(color), 0.0, 255.0));
      }
    }
  }
}

void layout_native_content(HWND window, FrameState& state) {
  if (state.reshaping || !IsWindow(state.widget) || IsIconic(window)) return;
  state.reshaping = true;
  RECT client{};
  GetClientRect(window, &client);
  const bool overlay = state.titlebar_controls && state.surface.has_value();
  const int top = overlay ? 0 : frame_scale(window, kChromeHeight);
  const int width = client.right > 0 ? client.right : 1;
  const int height = (client.bottom - top) > 0 ? client.bottom - top : 1;
  // Moving the actual WebView child also updates the controller bounds through
  // its WM_SIZE handler. These are window coordinates, never CSS padding.
  // A synchronized wide workspace draws its toolbar in the caption itself.
  // Compact layouts retain a separate native caption above the WebView.
  MoveWindow(state.widget, 0, top, width, height, TRUE);
  if (overlay) {
    const HRGN region = CreateRectRgn(0, 0, width, height);
    const HRGN buttons = CreateRectRgn((std::max)(0, width - frame_scale(window, 160)),
        0, width, frame_scale(window, kChromeHeight));
    CombineRgn(region, region, buttons, RGN_DIFF);
    DeleteObject(buttons);
    // Keep the restored window's top resize edge native as well.
    if (!IsZoomed(window)) {
      const int edge = frame_metric(window, SM_CYFRAME) + frame_metric(window, SM_CXPADDEDBORDER);
      const HRGN resize = CreateRectRgn(0, 0, width, edge);
      CombineRgn(region, region, resize, RGN_DIFF);
      DeleteObject(resize);
    }
    if (!SetWindowRgn(state.widget, region, TRUE)) DeleteObject(region);
  } else {
    SetWindowRgn(state.widget, nullptr, TRUE);
  }
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
  const int top = frame_scale(window, kChromeHeight);
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
  const auto* state = frame_state(window);
  GdiFlush();
  if (state && state->surface) {
    paint_caption_surface(window, client, top, static_cast<unsigned char*>(pixels), *state->surface);
  }
  // The caption joins the AI surface without repeating panel names. Window
  // context remains available to task switching through SetWindowTextW.
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
    if (dwm_handled && (dwm_result == HTMINBUTTON || dwm_result == HTMAXBUTTON ||
                        dwm_result == HTCLOSE)) return dwm_result;
    // A custom client frame can make DWM report HTCLIENT after maximizing.
    // Resolve the actual DWM button bounds before falling back to the content
    // or draggable caption, using screen coordinates throughout.
    const auto button = caption_button_hit_test(window, lparam);
    if (button != HTNOWHERE) return button;
    if (dwm_handled && dwm_result != HTCLIENT && dwm_result != HTNOWHERE) return dwm_result;
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
    RECT client{};
    GetClientRect(window, &client);
    // Empty space next to the system buttons is not draggable. The button
    // rectangles above already returned their native non-client hit codes.
    if (point.x >= client.right - frame_scale(window, 160)) return HTCLIENT;
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
      // Keep DWM's caption strip at the same DPI-scaled height as its WebView
      // exclusion and hit targets after moving between displays.
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

bool enable_native_caption_regions(webview_t view) {
  auto* controller = static_cast<ICoreWebView2Controller*>(
      webview_get_native_handle(view, WEBVIEW_NATIVE_HANDLE_KIND_BROWSER_CONTROLLER));
  if (!controller) return false;
  ICoreWebView2* core = nullptr;
  if (FAILED(controller->get_CoreWebView2(&core)) || !core) return false;
  ICoreWebView2Settings* settings = nullptr;
  const auto settings_result = core->get_Settings(&settings);
  core->Release();
  if (FAILED(settings_result) || !settings) return false;
  ICoreWebView2Settings9* caption_settings = nullptr;
  const auto query_result = settings->QueryInterface(IID_ICoreWebView2Settings9,
      reinterpret_cast<void**>(&caption_settings));
  settings->Release();
  if (FAILED(query_result) || !caption_settings) return false;
  const auto enabled = caption_settings->put_IsNonClientRegionSupportEnabled(TRUE);
  caption_settings->Release();
  return SUCCEEDED(enabled);
}

bool install_fused_frame(webview_t view) {
  auto window = static_cast<HWND>(webview_get_window(view));
  auto widget = static_cast<HWND>(webview_get_native_handle(view, WEBVIEW_NATIVE_HANDLE_KIND_UI_WIDGET));
  if (window == nullptr || widget == nullptr || frame_state(window) != nullptr) return false;
  auto state = std::make_unique<FrameState>();
  state->widget = widget;
  state->titlebar_controls = enable_native_caption_regions(view);
  if (!SetPropW(window, kFrameProperty, state.get())) return false;
  SetLastError(0);
  state->previous = reinterpret_cast<WNDPROC>(
      SetWindowLongPtrW(window, GWLP_WNDPROC, reinterpret_cast<LONG_PTR>(fused_frame_proc)));
  if (state->previous == nullptr) { RemovePropW(window, kFrameProperty); return false; }
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
  return installed->titlebar_controls;
}

// This channel changes presentation only. Future native actions must route back
// to the existing application commands, never create a second execution path.
IpcReply update_shell_context(webview_t view, const nlohmann::json& params) {
  const auto label = window_context_label(params);
  if (!label) return {false, {{"code", "INVALID_REQUEST"}, {"message", "Window context is invalid"}}};
  const auto window = static_cast<HWND>(webview_get_window(view));
  auto* state = window == nullptr ? nullptr : frame_state(window);
  if (state == nullptr) return {false, {{"code", "UNAVAILABLE"}, {"message", "Native window shell is unavailable"}}};
  const std::wstring window_title = L"Yukinal  |  " + *label;
  SetWindowTextW(window, window_title.c_str());
  InvalidateRect(window, nullptr, FALSE);
  return {true, nlohmann::json::object()};
}

IpcReply update_caption_surface(webview_t view, const nlohmann::json& params) {
  const auto invalid = [] { return IpcReply{false,
      {{"code", "INVALID_REQUEST"}, {"message", "Caption surface is invalid"}}}; };
  std::optional<CaptionSurface> surface;
  if (!params.is_null()) {
    if (!params.is_object() || params.size() != 7) return invalid();
    for (const auto* key : {"viewportWidth", "left", "right", "radius", "inspectorLeft"}) {
      if (!params.contains(key) || !params[key].is_number()) return invalid();
      const auto number = params[key].get<double>();
      if (!std::isfinite(number) || number < 0 || number > 32768) return invalid();
    }
    static const std::regex hex_color("^#[0-9a-fA-F]{6}$");
    for (const auto* key : {"background", "border"}) {
      if (!params.contains(key) || !params[key].is_string() ||
          !std::regex_match(params[key].get<std::string>(), hex_color)) return invalid();
    }
    const auto color = [&params](const char* key) {
      const auto value = std::stoul(params[key].get<std::string>().substr(1), nullptr, 16);
      return RGB((value >> 16) & 255, (value >> 8) & 255, value & 255);
    };
    CaptionSurface candidate{params["viewportWidth"].get<double>(), params["left"].get<double>(),
        params["right"].get<double>(), params["radius"].get<double>(), params["inspectorLeft"].get<double>(), color("background"), color("border")};
    if (candidate.viewport_width <= 0 || candidate.left >= candidate.right ||
        candidate.right > candidate.inspector_left || candidate.inspector_left > candidate.viewport_width || candidate.radius > 64) return invalid();
    // Native caption buttons always keep their own surface and hit targets.
    const auto window = static_cast<HWND>(webview_get_window(view));
    RECT client{};
    if (!window || !GetClientRect(window, &client)) return invalid();
    if (candidate.right * client.right / candidate.viewport_width <= client.right - frame_scale(window, 160)) {
      surface = candidate;
    }
  }
  const auto window = static_cast<HWND>(webview_get_window(view));
  auto* state = window == nullptr ? nullptr : frame_state(window);
  if (!state) return {false, {{"code", "UNAVAILABLE"}, {"message", "Native window shell is unavailable"}}};
  state->surface = surface;
  layout_native_content(window, *state);
  RECT caption{};
  GetClientRect(window, &caption);
  caption.bottom = frame_scale(window, kChromeHeight);
  InvalidateRect(window, &caption, FALSE);
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

}  // namespace yukinal::desktop_host

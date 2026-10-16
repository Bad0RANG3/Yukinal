#include "yukinal/desktop_host/windows_window_frame.hpp"
#include "yukinal/desktop_host/window_context.hpp"
#include <dwmapi.h>
#include <commctrl.h>
#include <uxtheme.h>
#include <algorithm>
#include <memory>
#include <utility>
#include <array>
#include <cwctype>
#include <vector>
#include <windowsx.h>

namespace yukinal::desktop_host {
constexpr COLORREF kOuterColor = RGB(0x14, 0x15, 0x18);
constexpr COLORREF kTitleColor = RGB(0x11, 0x12, 0x15);
constexpr COLORREF kSidebarColor = RGB(0x19, 0x1b, 0x20);
constexpr wchar_t kFrameProperty[] = L"YukinalFrame";
constexpr int kChromeHeight = 40;
constexpr int kSidebarWidth = 240;
constexpr int kContentInset = 8;
constexpr int kContentRadius = 14;

LRESULT CALLBACK sidebar_control_proc(HWND, UINT, WPARAM, LPARAM, UINT_PTR, DWORD_PTR);
constexpr std::array<const char*, 5> kSections{"servers", "projects", "tasks", "activity", "settings"};
constexpr std::array<const wchar_t*, 5> kSectionLabels{L"服务器", L"项目", L"排查任务", L"动态", L"设置"};
constexpr int kNavigationId = 1000;
constexpr int kAddId = 1010, kRefreshId = 1011, kSearchId = 1012, kListId = 1013;

struct FrameState {
  webview_t view = nullptr;
  HWND widget = nullptr;
  WNDPROC previous = nullptr;
  bool reshaping = false;
  bool sidebar_ready = false;
  std::wstring context_label;
  std::string section = "servers";
  WindowSidebar sidebar;
  std::vector<size_t> visible_servers;
  std::array<HWND, 5> navigation{};
  HWND search = nullptr, list = nullptr, add = nullptr, refresh = nullptr, hovered = nullptr;
  int caption_hovered = -1, caption_pressed = -1;
  std::vector<HWND> controls;
  HFONT font = nullptr, detail_font = nullptr;
  HBRUSH outer_brush = CreateSolidBrush(kOuterColor);
  HBRUSH sidebar_brush = CreateSolidBrush(kSidebarColor);
  HBRUSH entry_brush = CreateSolidBrush(RGB(0x1d, 0x1e, 0x23));

  ~FrameState() {
    for (const HWND control : controls) if (IsWindow(control)) {
      RemoveWindowSubclass(control, sidebar_control_proc, 1);
      DestroyWindow(control);
    }
    if (font) DeleteObject(font);
    if (detail_font) DeleteObject(detail_font);
    DeleteObject(outer_brush);
    DeleteObject(sidebar_brush);
    DeleteObject(entry_brush);
  }
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

int chrome_height(HWND window) {
  return frame_scale(window, kChromeHeight);
}

int caption_button(HWND window, POINT client_point) {
  RECT client{};
  GetClientRect(window, &client);
  const int width = frame_scale(window, 46), start = client.right - 3 * width;
  if (client_point.y < 0 || client_point.y >= chrome_height(window) || client_point.x < start || client_point.x >= client.right) return -1;
  return (client_point.x - start) / width;
}

int caption_button_at_screen(HWND window, LPARAM position) {
  POINT point{GET_X_LPARAM(position), GET_Y_LPARAM(position)};
  ScreenToClient(window, &point);
  return caption_button(window, point);
}

void update_sidebar_fonts(HWND window, FrameState& state) {
  if (state.font) DeleteObject(state.font);
  if (state.detail_font) DeleteObject(state.detail_font);
  state.font = CreateFontW(-frame_scale(window, 13), 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE,
      DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, DEFAULT_PITCH, L"Segoe UI");
  state.detail_font = CreateFontW(-frame_scale(window, 11), 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE,
      DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, DEFAULT_PITCH, L"Segoe UI");
  for (const auto control : state.controls) SendMessageW(control, WM_SETFONT, reinterpret_cast<WPARAM>(state.font), TRUE);
  SendMessageW(state.list, LB_SETITEMHEIGHT, 0, frame_scale(window, 60));
}

void emit_sidebar_action(FrameState& state, const nlohmann::json& action) {
  if (!state.sidebar_ready || state.sidebar.blocked) return;
  const auto script = "window.__YUKINAL_HOST__?.windowShell?.dispatchAction(" + action.dump() + ");";
  webview_eval(state.view, script.c_str());
}

void rebuild_server_rows(HWND window, FrameState& state) {
  const int length = GetWindowTextLengthW(state.search);
  std::wstring filter(static_cast<size_t>(length) + 1, L'\0');
  GetWindowTextW(state.search, filter.data(), length + 1);
  filter.resize(static_cast<size_t>(length));
  std::transform(filter.begin(), filter.end(), filter.begin(), towlower);
  const auto old_top = SendMessageW(state.list, LB_GETTOPINDEX, 0, 0);
  SendMessageW(state.list, WM_SETREDRAW, FALSE, 0);
  SendMessageW(state.list, LB_RESETCONTENT, 0, 0);
  state.visible_servers.clear();
  for (size_t i = 0; i < state.sidebar.servers.size(); ++i) {
    const auto& server = state.sidebar.servers[i];
    auto text = server.name + L" " + server.detail;
    std::transform(text.begin(), text.end(), text.begin(), towlower);
    if (!filter.empty() && text.find(filter) == std::wstring::npos) continue;
    const auto row = state.visible_servers.size();
    state.visible_servers.push_back(i);
    const auto* status_label = server.status == "connected" ? L"已连接" : server.status == "connecting" ? L"连接中" : server.status == "error" ? L"连接错误" : L"未连接";
    const auto accessible_name = server.name + L"，" + server.detail + L"，" + status_label;
    SendMessageW(state.list, LB_ADDSTRING, 0, reinterpret_cast<LPARAM>(accessible_name.c_str()));
    if (state.sidebar.selected_server_id == server.id) SendMessageW(state.list, LB_SETCURSEL, row, 0);
  }
  if (old_top != LB_ERR) SendMessageW(state.list, LB_SETTOPINDEX, old_top, 0);
  SendMessageW(state.list, WM_SETREDRAW, TRUE, 0);
  InvalidateRect(state.list, nullptr, TRUE);
  InvalidateRect(window, nullptr, FALSE);
}

LRESULT CALLBACK sidebar_control_proc(HWND control, UINT message, WPARAM wparam, LPARAM lparam,
                                      UINT_PTR, DWORD_PTR reference) {
  auto& state = *reinterpret_cast<FrameState*>(reference);
  if (message == WM_MOUSEMOVE && state.hovered != control) {
    const auto previous = state.hovered;
    state.hovered = control;
    if (previous) InvalidateRect(previous, nullptr, FALSE);
    InvalidateRect(control, nullptr, FALSE);
    TRACKMOUSEEVENT track{sizeof(track), TME_LEAVE, control, 0};
    TrackMouseEvent(&track);
  } else if (message == WM_MOUSELEAVE && state.hovered == control) {
    state.hovered = nullptr;
    InvalidateRect(control, nullptr, FALSE);
  } else if (message == WM_KEYDOWN && wparam == VK_TAB) {
    const auto current = std::find(state.controls.begin(), state.controls.end(), control);
    const bool backwards = (GetKeyState(VK_SHIFT) & 0x8000) != 0;
    auto index = std::distance(state.controls.begin(), current);
    for (size_t i = 0; i < state.controls.size(); ++i) {
      index += backwards ? -1 : 1;
      if (index < 0 || index >= static_cast<ptrdiff_t>(state.controls.size())) {
        SetFocus(state.widget);
        return 0;
      }
      const auto next = state.controls[static_cast<size_t>(index)];
      if (IsWindowEnabled(next) && IsWindowVisible(next)) { SetFocus(next); return 0; }
    }
    return 0;
  } else if (message == WM_KEYDOWN && wparam == VK_ESCAPE && control == state.search) {
    SetWindowTextW(control, L"");
    return 0;
  }
  const auto result = DefSubclassProc(control, message, wparam, lparam);
  if (message == WM_SETFOCUS || message == WM_KILLFOCUS) InvalidateRect(control, nullptr, FALSE);
  if (message == WM_PAINT && control == state.list && state.visible_servers.empty()) {
    const auto dc = GetDC(control);
    RECT area{};
    GetClientRect(control, &area);
    area.top += frame_scale(GetParent(control), 26);
    const auto previous = SelectObject(dc, state.detail_font);
    SetTextColor(dc, RGB(143, 147, 158));
    SetBkMode(dc, TRANSPARENT);
    const auto* text = !state.sidebar_ready || state.sidebar.loading ? L"正在加载服务器…" :
        state.sidebar.failed ? L"无法加载服务器，请刷新重试" :
        state.sidebar.servers.empty() ? L"添加服务器，开始连接远程环境" : L"没有匹配的服务器";
    DrawTextW(dc, text, -1, &area, DT_CENTER | DT_TOP | DT_WORDBREAK | DT_NOPREFIX);
    SelectObject(dc, previous);
    ReleaseDC(control, dc);
  } else if (message == WM_PAINT && control == state.search && GetWindowTextLengthW(control) == 0 && GetFocus() != control) {
    const auto dc = GetDC(control);
    RECT area{};
    GetClientRect(control, &area);
    const auto previous = SelectObject(dc, state.font);
    SetBkMode(dc, TRANSPARENT);
    SetTextColor(dc, RGB(125, 129, 142));
    DrawTextW(dc, L"搜索名称或主机", -1, &area, DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
    SelectObject(dc, previous);
    ReleaseDC(control, dc);
  }
  return result;
}

bool create_sidebar_controls(HWND window, FrameState& state) {
  const auto instance = reinterpret_cast<HINSTANCE>(GetWindowLongPtrW(window, GWLP_HINSTANCE));
  const auto button = [&](const wchar_t* label, int id) {
    return CreateWindowExW(0, L"BUTTON", label, WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW,
        0, 0, 1, 1, window, reinterpret_cast<HMENU>(static_cast<INT_PTR>(id)), instance, nullptr);
  };
  for (size_t i = 0; i < state.navigation.size(); ++i) state.navigation[i] = button(kSectionLabels[i], kNavigationId + static_cast<int>(i));
  state.add = button(L"添加服务器", kAddId);
  state.refresh = button(L"刷新", kRefreshId);
  state.search = CreateWindowExW(0, L"EDIT", L"", WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL,
      0, 0, 1, 1, window, reinterpret_cast<HMENU>(kSearchId), instance, nullptr);
  SendMessageW(state.search, EM_SETCUEBANNER, TRUE, reinterpret_cast<LPARAM>(L"搜索名称或主机"));
  SendMessageW(state.search, EM_SETLIMITTEXT, 256, 0);
  state.list = CreateWindowExW(0, L"LISTBOX", L"服务器列表",
      WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_VSCROLL | LBS_NOTIFY | LBS_OWNERDRAWFIXED | LBS_HASSTRINGS | LBS_NOINTEGRALHEIGHT,
      0, 0, 1, 1, window, reinterpret_cast<HMENU>(kListId), instance, nullptr);
  SetWindowTheme(state.list, L"DarkMode_Explorer", nullptr);
  state.controls.assign(state.navigation.begin(), state.navigation.begin() + 4);
  state.controls.insert(state.controls.end(), {state.search, state.list, state.refresh, state.add, state.navigation[4]});
  if (std::any_of(state.controls.begin(), state.controls.end(), [](HWND control) { return control == nullptr; })) return false;
  for (const auto control : state.controls) {
    if (!SetWindowSubclass(control, sidebar_control_proc, 1, reinterpret_cast<DWORD_PTR>(&state))) return false;
    EnableWindow(control, FALSE);
  }
  update_sidebar_fonts(window, state);
  return true;
}

void draw_sidebar_item(HWND window, FrameState& state, const DRAWITEMSTRUCT& item) {
  const auto dc = item.hDC;
  RECT bounds = item.rcItem;
  const bool selected = item.CtlID == kListId ? (item.itemState & ODS_SELECTED) != 0 :
      item.CtlID >= kNavigationId && item.CtlID < kNavigationId + 5 && state.section == kSections[item.CtlID - kNavigationId];
  const bool highlight = selected || (item.itemState & ODS_SELECTED) || state.hovered == item.hwndItem;
  FillRect(dc, &bounds, state.sidebar_brush);
  if (highlight) {
    const auto brush = CreateSolidBrush(selected ? RGB(0x2b, 0x2d, 0x34) : RGB(0x21, 0x23, 0x29));
    const auto old_brush = SelectObject(dc, brush);
    const auto old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
    RoundRect(dc, bounds.left, bounds.top, bounds.right, bounds.bottom, frame_scale(window, 12), frame_scale(window, 12));
    SelectObject(dc, old_pen);
    SelectObject(dc, old_brush);
    DeleteObject(brush);
  }
  const auto old_font = SelectObject(dc, state.font);
  SetBkMode(dc, TRANSPARENT);
  SetTextColor(dc, (item.itemState & ODS_DISABLED) ? RGB(106, 109, 119) : selected ? RGB(244, 244, 248) : RGB(192, 195, 205));
  bounds.left += frame_scale(window, 12);
  bounds.right -= frame_scale(window, 12);
  if (item.CtlID == kListId && item.itemID < state.visible_servers.size()) {
    const auto& server = state.sidebar.servers[state.visible_servers[item.itemID]];
    RECT name = bounds;
    name.top += frame_scale(window, 8);
    name.bottom = name.top + frame_scale(window, 22);
    name.right -= frame_scale(window, 16);
    DrawTextW(dc, server.name.c_str(), -1, &name, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
    const auto dot = CreateSolidBrush(server.status == "connected" ? RGB(100, 192, 151) :
        server.status == "error" ? RGB(224, 130, 130) : server.status == "connecting" ? RGB(225, 182, 101) : RGB(108, 111, 122));
    RECT status{bounds.right - frame_scale(window, 7), name.top + frame_scale(window, 8), bounds.right, name.top + frame_scale(window, 15)};
    FillRect(dc, &status, dot);
    DeleteObject(dot);
    SelectObject(dc, state.detail_font);
    SetTextColor(dc, RGB(143, 147, 158));
    bounds.top += frame_scale(window, 32);
    DrawTextW(dc, server.detail.c_str(), -1, &bounds, DT_SINGLELINE | DT_TOP | DT_END_ELLIPSIS | DT_NOPREFIX);
  } else if (item.CtlID != kListId) {
    wchar_t label[80]{};
    GetWindowTextW(item.hwndItem, label, 80);
    DrawTextW(dc, label, -1, &bounds, DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX |
        (item.CtlID >= kNavigationId && item.CtlID < kNavigationId + 5 ? DT_LEFT : DT_CENTER));
  }
  if ((item.itemState & ODS_FOCUS) != 0) {
    RECT focus = item.rcItem;
    InflateRect(&focus, -3, -3);
    DrawFocusRect(dc, &focus);
  }
  SelectObject(dc, old_font);
}

void server_context_menu(HWND window, FrameState& state, LPARAM position) {
  if (!state.sidebar_ready || state.sidebar.blocked) return;
  POINT point{GET_X_LPARAM(position), GET_Y_LPARAM(position)};
  auto selected = SendMessageW(state.list, LB_GETCURSEL, 0, 0);
  if (point.x != -1 || point.y != -1) {
    auto local = point;
    ScreenToClient(state.list, &local);
    const auto hit = SendMessageW(state.list, LB_ITEMFROMPOINT, 0, MAKELPARAM(local.x, local.y));
    if (HIWORD(hit)) return;
    selected = LOWORD(hit);
  } else {
    RECT row{};
    if (selected == LB_ERR || SendMessageW(state.list, LB_GETITEMRECT, selected, reinterpret_cast<LPARAM>(&row)) == LB_ERR) return;
    point = {row.left, row.bottom};
    ClientToScreen(state.list, &point);
  }
  if (selected == LB_ERR || static_cast<size_t>(selected) >= state.visible_servers.size()) return;
  // The popup runs a nested message loop. Copy the identity before opening it;
  // a cache update may replace/reorder the snapshot while the menu is visible.
  const auto server = state.sidebar.servers[state.visible_servers[static_cast<size_t>(selected)]];
  const auto menu = CreatePopupMenu();
  AppendMenuW(menu, MF_STRING, 1, L"编辑服务器");
  AppendMenuW(menu, MF_STRING | (server.status == "connecting" ? MF_GRAYED : 0), 2,
      server.status == "connected" ? L"断开连接" : L"连接服务器");
  AppendMenuW(menu, MF_SEPARATOR, 0, nullptr);
  AppendMenuW(menu, MF_STRING, 3, L"删除服务器…");
  const auto command = TrackPopupMenuEx(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, point.x, point.y, window, nullptr);
  DestroyMenu(menu);
  if (command) emit_sidebar_action(state, {{"type", command == 1 ? "edit-server" : command == 3 ? "delete-server" :
      server.status == "connected" ? "disconnect-server" : "connect-server"}, {"serverId", server.id}});
}

void layout_native_content(HWND window, FrameState& state) {
  if (state.reshaping || !IsWindow(state.widget) || IsIconic(window)) return;
  state.reshaping = true;
  RECT client{};
  GetClientRect(window, &client);
  const auto layout = window_layout(client.right, client.bottom, chrome_height(window), frame_scale(window, kContentInset), frame_scale(window, kSidebarWidth));
  MoveWindow(state.widget, layout.content_x, layout.content_y, layout.content_width, layout.content_height, TRUE);
  const int diameter = frame_scale(window, 2 * kContentRadius);
  const HRGN region = CreateRoundRectRgn(0, 0, layout.content_width + 1, layout.content_height + 1, diameter, diameter);
  if (region != nullptr && !SetWindowRgn(state.widget, region, TRUE)) DeleteObject(region);
  const int pad = frame_scale(window, 16), gap = frame_scale(window, 6), row = frame_scale(window, 36);
  const int width = (std::max)(1, layout.sidebar_width - 2 * pad);
  for (int i = 0; i < 4; ++i) MoveWindow(state.navigation[i], pad,
      layout.content_y + frame_scale(window, 30) + i * (row + gap), width, row, TRUE);
  MoveWindow(state.navigation[4], pad, client.bottom - pad - row, width, row, TRUE);
  const int tools_y = layout.content_y + frame_scale(window, 222);
  const int add_width = frame_scale(window, 108);
  const int refresh_width = frame_scale(window, 54);
  const int tools_gap = frame_scale(window, 8);
  const int add_x = layout.sidebar_width - pad - add_width;
  MoveWindow(state.add, add_x, tools_y, add_width, frame_scale(window, 30), TRUE);
  MoveWindow(state.refresh, add_x - tools_gap - refresh_width, tools_y, refresh_width, frame_scale(window, 30), TRUE);
  MoveWindow(state.search, pad + frame_scale(window, 12), tools_y + frame_scale(window, 50), (std::max)(1, width - frame_scale(window, 24)), frame_scale(window, 24), TRUE);
  const int list_top = tools_y + frame_scale(window, 104);
  MoveWindow(state.list, pad, list_top, width, (std::max)(1, static_cast<int>(client.bottom) - pad - row - frame_scale(window, 18) - list_top), TRUE);
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
  const int top = chrome_height(window);
  // Give the caption and the sidebar their own shades so the functional zones
  // read as separate surfaces instead of one continuous L-shaped frame.
  const HBRUSH title_fill = CreateSolidBrush(kTitleColor);
  const RECT title_band{0, 0, client.right, top};
  FillRect(window_dc, &title_band, title_fill);
  DeleteObject(title_fill);
  const HBRUSH sidebar_fill = CreateSolidBrush(kSidebarColor);
  const RECT sidebar_band{0, top, frame_scale(window, kSidebarWidth), client.bottom};
  FillRect(window_dc, &sidebar_band, sidebar_fill);
  DeleteObject(sidebar_fill);
  const HPEN border = CreatePen(PS_SOLID, 1, RGB(54, 54, 56));
  const HGDIOBJ previous_pen = SelectObject(window_dc, border);
  const HGDIOBJ previous_brush = SelectObject(window_dc, GetStockObject(HOLLOW_BRUSH));
  const auto layout = window_layout(client.right, client.bottom, top, inset, frame_scale(window, kSidebarWidth));
  RoundRect(window_dc, layout.content_x - 1, top - 1, client.right - inset + 1,
      client.bottom - inset + 1, frame_scale(window, kContentRadius * 2),
      frame_scale(window, kContentRadius * 2));
  SelectObject(window_dc, previous_brush);
  const HPEN divider = CreatePen(PS_SOLID, 1, RGB(0x2c, 0x2e, 0x35));
  SelectObject(window_dc, divider);
  MoveToEx(window_dc, layout.sidebar_width - 1, top, nullptr);
  LineTo(window_dc, layout.sidebar_width - 1, client.bottom);
  MoveToEx(window_dc, 0, top - 1, nullptr);
  LineTo(window_dc, layout.sidebar_width, top - 1);
  SelectObject(window_dc, previous_pen);
  DeleteObject(divider);
  DeleteObject(border);
  auto* sidebar_state = frame_state(window);
  if (sidebar_state != nullptr && sidebar_state->font != nullptr) {
    const HGDIOBJ old_font = SelectObject(window_dc, sidebar_state->detail_font);
    SetBkMode(window_dc, TRANSPARENT);
    SetTextColor(window_dc, RGB(143, 147, 158));
    RECT eyebrow{frame_scale(window, 28), top + frame_scale(window, 8), layout.sidebar_width, top + frame_scale(window, 28)};
    DrawTextW(window_dc, L"工作区", -1, &eyebrow, DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
    RECT heading{frame_scale(window, 28), top + frame_scale(window, 222), layout.sidebar_width - frame_scale(window, 140), top + frame_scale(window, 252)};
    DrawTextW(window_dc, L"服务器", -1, &heading, DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
    RECT search{frame_scale(window, 16), top + frame_scale(window, 266), layout.sidebar_width - frame_scale(window, 16), top + frame_scale(window, 302)};
    const HGDIOBJ fill = SelectObject(window_dc, sidebar_state->entry_brush);
    const HGDIOBJ no_pen = SelectObject(window_dc, GetStockObject(NULL_PEN));
    RoundRect(window_dc, search.left, search.top, search.right, search.bottom, frame_scale(window, 10), frame_scale(window, 10));
    SelectObject(window_dc, no_pen);
    SelectObject(window_dc, fill);
    const auto count = std::to_wstring(sidebar_state->visible_servers.size()) + L" / " + std::to_wstring(sidebar_state->sidebar.servers.size()) + L" 台服务器";
    RECT meta{frame_scale(window, 28), top + frame_scale(window, 306), layout.sidebar_width - frame_scale(window, 16), top + frame_scale(window, 326)};
    DrawTextW(window_dc, count.c_str(), -1, &meta, DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
    SelectObject(window_dc, old_font);
  }
  // Composite an opaque native caption. Its controls use the same 40-DIP
  // height and call system commands; WebView never paints window controls.
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
  const HBRUSH caption_fill = CreateSolidBrush(kTitleColor);
  FillRect(dc, &caption, caption_fill);
  DeleteObject(caption_fill);
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
  RECT title{layout.content_x + frame_scale(window, 24), 0,
             client.right - frame_scale(window, 160), top};
  const auto* state = frame_state(window);
  DrawTextW(dc, state ? state->context_label.c_str() : L"", -1, &title,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
  const int button_width = frame_scale(window, 46);
  for (int i = 0; i < 3; ++i) {
    RECT button{client.right - (3 - i) * button_width, 0, client.right - (2 - i) * button_width, top};
    if (state && (state->caption_hovered == i || state->caption_pressed == i)) {
      const auto hover = CreateSolidBrush(i == 2 ? RGB(196, 43, 54) : RGB(48, 50, 57));
      FillRect(dc, &button, hover);
      DeleteObject(hover);
    }
    const int center_x = (button.left + button.right) / 2, center_y = top / 2;
    const int half = frame_scale(window, 5);
    const auto pen = CreatePen(PS_SOLID, (std::max)(1, frame_scale(window, 1)), active ? RGB(222, 224, 232) : RGB(145, 148, 158));
    const auto previous_line = SelectObject(dc, pen);
    const auto previous_fill = SelectObject(dc, GetStockObject(HOLLOW_BRUSH));
    if (i == 0) { MoveToEx(dc, center_x - half, center_y, nullptr); LineTo(dc, center_x + half, center_y); }
    else if (i == 2) {
      MoveToEx(dc, center_x - half, center_y - half, nullptr); LineTo(dc, center_x + half, center_y + half);
      MoveToEx(dc, center_x + half, center_y - half, nullptr); LineTo(dc, center_x - half, center_y + half);
    } else if (IsZoomed(window)) {
      const int shift = frame_scale(window, 2);
      MoveToEx(dc, center_x - half + shift, center_y - half + shift, nullptr);
      LineTo(dc, center_x - half + shift, center_y - half);
      LineTo(dc, center_x + half, center_y - half);
      LineTo(dc, center_x + half, center_y + half - shift);
      LineTo(dc, center_x + half - shift, center_y + half - shift);
      Rectangle(dc, center_x - half, center_y - half + shift, center_x + half - shift + 1, center_y + half + 1);
    } else Rectangle(dc, center_x - half, center_y - half, center_x + half + 1, center_y + half + 1);
    SelectObject(dc, previous_fill);
    SelectObject(dc, previous_line);
    DeleteObject(pen);
  }
  SelectObject(dc, previous_font);
  DeleteObject(context_font);
  DeleteObject(font);
  GdiFlush();
  auto* bytes = static_cast<unsigned char*>(pixels);
  for (size_t pixel = 0; pixel < static_cast<size_t>(client.right) * top; ++pixel) bytes[pixel * 4 + 3] = 255;
  BitBlt(window_dc, 0, 0, client.right, top, dc, 0, 0, SRCCOPY);
  SelectObject(dc, previous_bitmap);
  DeleteDC(dc);
  DeleteObject(bitmap);
  EndPaint(window, &paint);
}

LRESULT CALLBACK fused_frame_proc(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
  auto* state = frame_state(window);
  if (state == nullptr || state->previous == nullptr) return DefWindowProcW(window, message, wparam, lparam);

  if (message == WM_NCHITTEST) {
    const int button = caption_button_at_screen(window, lparam);
    if (button >= 0) return button == 0 ? HTMINBUTTON : button == 1 ? HTMAXBUTTON : HTCLOSE;
  }
  if (message == WM_NCMOUSEMOVE || message == WM_MOUSEMOVE) {
    const int button = message == WM_NCMOUSEMOVE ? caption_button_at_screen(window, lparam) :
        caption_button(window, {GET_X_LPARAM(lparam), GET_Y_LPARAM(lparam)});
    if (button != state->caption_hovered) { state->caption_hovered = button; InvalidateRect(window, nullptr, FALSE); }
    if (button >= 0) {
      TRACKMOUSEEVENT track{sizeof(track), static_cast<DWORD>(TME_LEAVE | (message == WM_NCMOUSEMOVE ? TME_NONCLIENT : 0)), window, 0};
      TrackMouseEvent(&track);
    }
  } else if (message == WM_NCMOUSELEAVE || message == WM_MOUSELEAVE) {
    state->caption_hovered = -1;
    InvalidateRect(window, nullptr, FALSE);
  } else if (message == WM_NCLBUTTONDOWN) {
    const int button = caption_button_at_screen(window, lparam);
    if (button >= 0) {
      state->caption_pressed = button;
      SetCapture(window);
      InvalidateRect(window, nullptr, FALSE);
      return 0;
    }
  } else if (message == WM_LBUTTONUP && state->caption_pressed >= 0) {
    const int pressed = std::exchange(state->caption_pressed, -1);
    const int released = caption_button(window, {GET_X_LPARAM(lparam), GET_Y_LPARAM(lparam)});
    ReleaseCapture();
    InvalidateRect(window, nullptr, FALSE);
    if (pressed == released) PostMessageW(window, WM_SYSCOMMAND,
        pressed == 0 ? SC_MINIMIZE : pressed == 1 ? IsZoomed(window) ? SC_RESTORE : SC_MAXIMIZE : SC_CLOSE, 0);
    return 0;
  } else if (message == WM_CAPTURECHANGED || message == WM_CANCELMODE) {
    state->caption_pressed = -1;
    InvalidateRect(window, nullptr, FALSE);
  }

  // Native HTMAXBUTTON lets Windows own Snap hover. Window commands and
  // resize/system-menu behavior continue through the standard Win32 procedure.
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
    if (point.y < chrome_height(window)) return HTCAPTION;
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
  if (message == WM_DRAWITEM && lparam) {
    draw_sidebar_item(window, *state, *reinterpret_cast<DRAWITEMSTRUCT*>(lparam));
    return TRUE;
  }
  if (message == WM_MEASUREITEM && wparam == kListId) {
    reinterpret_cast<MEASUREITEMSTRUCT*>(lparam)->itemHeight = frame_scale(window, 60);
    return TRUE;
  }
  if (message == WM_CTLCOLOREDIT || message == WM_CTLCOLORLISTBOX || (message == WM_CTLCOLORSTATIC && reinterpret_cast<HWND>(lparam) == state->search)) {
    const auto dc = reinterpret_cast<HDC>(wparam);
    SetTextColor(dc, RGB(208, 211, 220));
    const bool edit = reinterpret_cast<HWND>(lparam) == state->search;
    SetBkColor(dc, edit ? RGB(0x1d, 0x1e, 0x23) : kSidebarColor);
    return reinterpret_cast<LRESULT>(edit ? state->entry_brush : state->sidebar_brush);
  }
  if (message == WM_CONTEXTMENU && reinterpret_cast<HWND>(wparam) == state->list) {
    server_context_menu(window, *state, lparam);
    return 0;
  }
  if (message == WM_COMMAND) {
    const auto id = LOWORD(wparam), notification = HIWORD(wparam);
    if (id == kSearchId && notification == EN_CHANGE) { rebuild_server_rows(window, *state); return 0; }
    if (id == kListId && notification == LBN_SELCHANGE) {
      const auto selected = SendMessageW(state->list, LB_GETCURSEL, 0, 0);
      if (selected != LB_ERR && static_cast<size_t>(selected) < state->visible_servers.size())
        emit_sidebar_action(*state, {{"type", "select-server"}, {"serverId", state->sidebar.servers[state->visible_servers[selected]].id}});
      return 0;
    }
    if (notification == BN_CLICKED) {
      if (id >= kNavigationId && id < kNavigationId + 5) emit_sidebar_action(*state, {{"type", "navigate"}, {"section", kSections[id - kNavigationId]}});
      else if (id == kAddId || id == kRefreshId) emit_sidebar_action(*state, {{"type", id == kAddId ? "add-server" : "refresh-servers"}});
      return 0;
    }
  }
  if (dwm_handled) return dwm_result;

  const auto result = CallWindowProcW(state->previous, window, message, wparam, lparam);
  if (message == WM_SIZE || message == WM_DPICHANGED) {
    layout_native_content(window, *state);
    if (message == WM_DPICHANGED) {
      update_sidebar_fonts(window, *state);
      const MARGINS glass{0, 0, chrome_height(window), 0};
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

bool install_fused_frame(webview_t view) {
  auto window = static_cast<HWND>(webview_get_window(view));
  auto widget = static_cast<HWND>(webview_get_native_handle(view, WEBVIEW_NATIVE_HANDLE_KIND_UI_WIDGET));
  if (window == nullptr || widget == nullptr) return false;
  if (frame_state(window) != nullptr) return true;
  auto state = std::make_unique<FrameState>();
  state->view = view;
  state->widget = widget;
  if (!SetPropW(window, kFrameProperty, state.get())) return false;
  state->previous = reinterpret_cast<WNDPROC>(
      SetWindowLongPtrW(window, GWLP_WNDPROC, reinterpret_cast<LONG_PTR>(fused_frame_proc)));
  if (state->previous == nullptr) { RemovePropW(window, kFrameProperty); return false; }
  if (!create_sidebar_controls(window, *state)) {
    SetWindowLongPtrW(window, GWLP_WNDPROC, reinterpret_cast<LONG_PTR>(state->previous));
    RemovePropW(window, kFrameProperty);
    return false;
  }
  auto* installed = state.release();
  // Keep standard window styles for system commands, accessibility and Snap.
  const auto style = GetWindowLongPtrW(window, GWL_STYLE);
  SetWindowLongPtrW(window, GWL_STYLE, style | WS_CAPTION | WS_THICKFRAME |
                    WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_SYSMENU | WS_CLIPCHILDREN);
  // DWM needs the entire caption strip to render the native window controls.
  const MARGINS glass{0, 0, chrome_height(window), 0};
  DwmExtendFrameIntoClientArea(window, &glass);
  const int corners = DWMWCP_ROUND;
  DwmSetWindowAttribute(window, DWMWA_WINDOW_CORNER_PREFERENCE, &corners, sizeof(corners));
  DwmSetWindowAttribute(window, DWMWA_CAPTION_COLOR, &kOuterColor, sizeof(kOuterColor));
  const BOOL dark = TRUE;
  DwmSetWindowAttribute(window, 20 /* DWMWA_USE_IMMERSIVE_DARK_MODE */, &dark, sizeof(dark));
  SetWindowPos(window, nullptr, 0, 0, 0, 0,
               SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
  layout_native_content(window, *installed);
  return true;
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
  state->section = params["section"].get<std::string>();
  for (const auto control : state->navigation) InvalidateRect(control, nullptr, TRUE);
  layout_native_content(window, *state);
  const std::wstring window_title = L"Yukinal  |  " + *label;
  SetWindowTextW(window, window_title.c_str());
  InvalidateRect(window, nullptr, FALSE);
  return {true, nlohmann::json::object()};
}

IpcReply update_shell_sidebar(webview_t view, const nlohmann::json& params) {
  auto sidebar = parse_window_sidebar(params);
  if (!sidebar) return {false, {{"code", "INVALID_REQUEST"}, {"message", "Window sidebar is invalid"}}};
  const auto window = static_cast<HWND>(webview_get_window(view));
  auto* state = window == nullptr ? nullptr : frame_state(window);
  if (state == nullptr) return {false, {{"code", "UNAVAILABLE"}, {"message", "Native sidebar is unavailable"}}};
  state->sidebar = std::move(*sidebar);
  state->sidebar_ready = true;
  for (const auto control : state->controls) EnableWindow(control, !state->sidebar.blocked);
  rebuild_server_rows(window, *state);
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

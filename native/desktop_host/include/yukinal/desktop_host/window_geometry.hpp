#pragma once
#include <algorithm>

namespace yukinal::desktop_host {
struct FrameRect { int left, top, right, bottom; };

enum class FrameResizeEdge { none, top, top_left, top_right };

// The restored resize strip takes precedence over caption buttons. Otherwise
// dragging the upper-right corner can click Close instead of resizing.
inline FrameResizeEdge frame_top_resize_edge(FrameRect window, int screen_x, int screen_y,
                                             int edge_x, int edge_y, bool maximized) {
  if (maximized || screen_x < window.left || screen_x >= window.right ||
      screen_y < window.top || screen_y >= window.top + edge_y) return FrameResizeEdge::none;
  if (screen_x < window.left + edge_x * 2) return FrameResizeEdge::top_left;
  if (screen_x >= window.right - edge_x * 2) return FrameResizeEdge::top_right;
  return FrameResizeEdge::top;
}

// Maximized windows include invisible resize borders beyond the monitor. Use
// the monitor's actual work area rather than assuming a particular frame size.
inline FrameRect frame_client_bounds(FrameRect window, FrameRect work_area,
                                     int edge_x, int edge_y, bool maximized) {
  if (maximized) return {
    (std::max)(window.left, work_area.left), (std::max)(window.top, work_area.top),
    (std::min)(window.right, work_area.right), (std::min)(window.bottom, work_area.bottom)
  };
  return {window.left + edge_x, window.top, window.right - edge_x, window.bottom - edge_y};
}
}  // namespace yukinal::desktop_host

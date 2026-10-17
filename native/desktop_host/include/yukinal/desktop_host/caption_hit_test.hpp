#pragma once

namespace yukinal::desktop_host {

struct CaptionButtonRect {
  int left;
  int top;
  int right;
  int bottom;
};

enum class CaptionButton { none, minimize, maximize, close };

// DWM returns window-relative bounds; WM_NCHITTEST supplies screen coordinates.
// Use the window origin, including its hidden maximized resize borders.
inline CaptionButton hit_test_caption_buttons(CaptionButtonRect bounds,
                                             int window_x, int window_y,
                                             int screen_x, int screen_y) {
  const int x = screen_x - window_x;
  const int y = screen_y - window_y;
  const int width = bounds.right - bounds.left;
  if (width < 3 || bounds.bottom <= bounds.top || x < bounds.left ||
      x >= bounds.right || y < bounds.top || y >= bounds.bottom) {
    return CaptionButton::none;
  }
  const int slot = (x - bounds.left) * 3 / width;
  return slot == 0 ? CaptionButton::minimize
       : slot == 1 ? CaptionButton::maximize : CaptionButton::close;
}

}  // namespace yukinal::desktop_host

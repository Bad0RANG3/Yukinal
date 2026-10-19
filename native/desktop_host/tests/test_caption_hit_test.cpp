#include "yukinal/desktop_host/caption_hit_test.hpp"
#include "yukinal/desktop_host/window_geometry.hpp"

#include <cstdlib>
#include <iostream>

namespace {
void check(bool condition, const char* message) {
  if (condition) return;
  std::cerr << message << '\n';
  std::exit(1);
}
}  // namespace

int main() {
  using yukinal::desktop_host::CaptionButton;
  using yukinal::desktop_host::CaptionButtonRect;
  using yukinal::desktop_host::hit_test_caption_buttons;

  const CaptionButtonRect restored{1302, 0, 1440, 40};
  check(hit_test_caption_buttons(restored, 200, 100, 1520, 120) == CaptionButton::minimize,
        "restored minimize must use the window's screen origin");
  check(hit_test_caption_buttons(restored, 200, 100, 1570, 120) == CaptionButton::maximize,
        "restored maximize must be a non-client button");
  check(hit_test_caption_buttons(restored, 200, 100, 1620, 120) == CaptionButton::close,
        "restored close must be a non-client button");

  // A maximized window extends beyond the monitor by its invisible resize edge.
  const CaptionButtonRect maximized{2110, 8, 2248, 40};
  check(hit_test_caption_buttons(maximized, -8, -8, 2120, 14) == CaptionButton::minimize,
        "maximized minimize must account for the hidden left and top borders");
  check(hit_test_caption_buttons(maximized, -8, -8, 2168, 14) == CaptionButton::maximize,
        "maximized restore must remain clickable");
  check(hit_test_caption_buttons(maximized, -8, -8, 2215, 14) == CaptionButton::close,
        "maximized close must remain clickable");

  const CaptionButtonRect scaled{1953, 0, 2160, 60};
  check(hit_test_caption_buttons(scaled, -2160, 80, -110, 110) == CaptionButton::maximize,
        "scaled buttons on a negative-origin display must retain their hit zones");
  check(hit_test_caption_buttons(restored, 0, 0, 1347, 20) == CaptionButton::minimize &&
        hit_test_caption_buttons(restored, 0, 0, 1348, 20) == CaptionButton::maximize &&
        hit_test_caption_buttons(restored, 0, 0, 1394, 20) == CaptionButton::close,
        "adjacent buttons must have distinct, contiguous bounds");
  check(hit_test_caption_buttons(restored, 0, 0, 1440, 20) == CaptionButton::none &&
        hit_test_caption_buttons(restored, 0, 0, 1400, 40) == CaptionButton::none &&
        hit_test_caption_buttons(restored, 0, 0, 1301, 20) == CaptionButton::none,
        "content and points beyond the buttons must not trigger window actions");
  check(hit_test_caption_buttons({0, 0, 0, 0}, 0, 0, 0, 0) == CaptionButton::none,
        "unavailable DWM bounds must not create a hit zone");

  using yukinal::desktop_host::frame_client_bounds;
  const auto full = frame_client_bounds({-8, -8, 1928, 1160}, {0, 0, 1920, 1152}, 8, 8, true);
  check(full.left == 0 && full.top == 0 && full.right == 1920 && full.bottom == 1152,
        "maximized content must stop at the visible work area on every edge");
  const auto secondary = frame_client_bounds({-2572, -12, 12, 1452}, {-2560, 32, 0, 1440}, 12, 12, true);
  check(secondary.left == -2560 && secondary.top == 32 && secondary.right == 0 && secondary.bottom == 1440,
        "scaled secondary displays and top taskbars must use their own work area");
  const auto normal = frame_client_bounds({200, 100, 1648, 1039}, {0, 0, 1920, 1152}, 8, 8, false);
  check(normal.left == 208 && normal.top == 100 && normal.right == 1640 && normal.bottom == 1031,
        "restored bounds must preserve the normal window geometry and caption origin");

  using yukinal::desktop_host::FrameResizeEdge;
  using yukinal::desktop_host::frame_top_resize_edge;
  check(frame_top_resize_edge({200, 100, 1648, 1039}, 1645, 102, 8, 8, false) == FrameResizeEdge::top_right &&
        frame_top_resize_edge({200, 100, 1648, 1039}, 202, 102, 8, 8, false) == FrameResizeEdge::top_left &&
        frame_top_resize_edge({200, 100, 1648, 1039}, 600, 102, 8, 8, false) == FrameResizeEdge::top,
        "restored top strip must resize both corners before testing caption buttons");
  check(frame_top_resize_edge({200, 100, 1648, 1039}, 1645, 120, 8, 8, false) == FrameResizeEdge::none &&
        frame_top_resize_edge({200, 100, 1648, 1039}, 1645, 102, 8, 8, true) == FrameResizeEdge::none &&
        frame_top_resize_edge({200, 100, 1648, 1039}, 1648, 102, 8, 8, false) == FrameResizeEdge::none,
        "buttons below the strip, maximized windows and outside points must retain their normal actions");
  check(frame_top_resize_edge({-2160, 80, 0, 1460}, -2, 82, 12, 12, false) == FrameResizeEdge::top_right,
        "scaled negative-origin windows must retain their corner resize zone");
  return 0;
}

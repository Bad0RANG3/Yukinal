#include "yukinal/desktop_host/caption_hit_test.hpp"

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
  return 0;
}

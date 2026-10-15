#pragma once

#include <nlohmann/json.hpp>

namespace yukinal::desktop_host {

// Resolve the current outbound proxy route for the settings view. Secret material
// is never included in this response.
[[nodiscard]] nlohmann::json resolve_network_proxy(bool system_mode);

}  // namespace yukinal::desktop_host

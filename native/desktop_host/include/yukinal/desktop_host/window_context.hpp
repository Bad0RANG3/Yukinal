#pragma once

#include <nlohmann/json.hpp>
#include <optional>
#include <string>

namespace yukinal::desktop_host {

// Presentation only. This parser cannot dispatch application commands.
std::optional<std::wstring> window_context_label(const nlohmann::json& params);

}  // namespace yukinal::desktop_host

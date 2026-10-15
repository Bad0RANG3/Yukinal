#include "yukinal/desktop_host/ipc_router.hpp"

#include <algorithm>
#include <cctype>
#include <cstdint>
#include <regex>
#include <utility>

#ifdef _WIN32
#include <windows.h>
#include <shellapi.h>
#else
#include <spawn.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <cerrno>
#include <thread>
#include <unistd.h>
extern char** environ;
#endif

namespace yukinal::desktop_host {
namespace {

IpcReply failure(std::string code, std::string message) {
  return {false, {{"code", std::move(code)}, {"message", std::move(message)}}};
}

bool exact_string_fields(const nlohmann::json& value,
                         const std::set<std::string>& fields) {
  if (!value.is_object() || value.size() != fields.size()) return false;
  return std::all_of(fields.begin(), fields.end(), [&value](const auto& field) {
    return value.contains(field) && value.at(field).is_string();
  });
}

bool valid_event_name(const std::string& value) {
  static const std::regex pattern(R"(^[a-z][a-z0-9_.-]{0,95}$)");
  return std::regex_match(value, pattern);
}

bool valid_subscription_token(const std::string& value) {
  static const std::regex pattern(R"(^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$)");
  return std::regex_match(value, pattern);
}

bool valid_method_name(const std::string& value) {
  static const std::regex pattern(R"(^[a-z][a-z0-9_.-]{0,127}$)");
  return std::regex_match(value, pattern);
}

bool valid_server_id(const std::string& value) {
  static const std::regex pattern(R"(^srv_[a-z0-9]+$)");
  return value.size() <= 256 && std::regex_match(value, pattern);
}

#ifdef _WIN32
std::wstring utf8_to_wide(const std::string& value) {
  const auto length = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS,
                                          value.data(), static_cast<int>(value.size()),
                                          nullptr, 0);
  if (length <= 0) return {};
  std::wstring result(static_cast<size_t>(length), L'\0');
  if (MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                          static_cast<int>(value.size()), result.data(), length) <= 0) {
    return {};
  }
  return result;
}
#endif

}  // namespace

IpcReply IpcRouter::invoke(const std::string& method, const nlohmann::json& params) {
  const bool internal_event_method =
      method == "__events.subscribe" || method == "__events.unsubscribe";
  if (!internal_event_method && !valid_method_name(method)) {
    return failure("INVALID_METHOD", "Desktop IPC command name is invalid");
  }
  if (!params.is_object()) {
    return failure("INVALID_PARAMS", "Desktop IPC parameters must be an object");
  }

  if (method == "core_ping") {
    if (!params.empty()) {
      return failure("INVALID_PARAMS", "core_ping does not accept parameters");
    }
    return {true, {{"version", YUKINAL_DESKTOP_VERSION}, {"os", operating_system()}}};
  }

  if (method == "server_list") {
    if (!params.empty()) {
      return failure("INVALID_PARAMS", "server_list does not accept parameters");
    }
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local server database is unavailable");
    }
    try {
      return {true, database_->list_servers()};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local server database could not read servers");
    }
  }

  if (method == "workspace_list") {
    if (!params.empty()) {
      return failure("INVALID_PARAMS", "workspace_list does not accept parameters");
    }
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local workspace database is unavailable");
    }
    try {
      return {true, database_->list_workspaces()};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local workspace database could not read workspaces");
    }
  }

  if (method == "activity_list") {
    for (auto item = params.begin(); item != params.end(); ++item) {
      if (item.key() != "serverId" && item.key() != "limit") {
        return failure("INVALID_PARAMS", "activity_list received an unexpected parameter");
      }
    }
    std::optional<std::string> server_id;
    if (params.contains("serverId")) {
      if (!params.at("serverId").is_string()) {
        return failure("INVALID_PARAMS", "activity_list serverId must be a string");
      }
      const auto value = params.at("serverId").get<std::string>();
      if (!valid_server_id(value)) {
        return failure("INVALID_PARAMS", "activity_list serverId is invalid");
      }
      server_id = value;
    }
    int limit = 50;
    if (params.contains("limit")) {
      const auto& value = params.at("limit");
      if (value.is_number_unsigned()) {
        const auto requested_limit = value.get<std::uint64_t>();
        if (requested_limit < 1 || requested_limit > 100) {
          return failure("INVALID_PARAMS", "activity_list limit must be between 1 and 100");
        }
        limit = static_cast<int>(requested_limit);
      } else if (value.is_number_integer()) {
        const auto requested_limit = value.get<std::int64_t>();
        if (requested_limit < 1 || requested_limit > 100) {
          return failure("INVALID_PARAMS", "activity_list limit must be between 1 and 100");
        }
        limit = static_cast<int>(requested_limit);
      } else {
        return failure("INVALID_PARAMS", "activity_list limit must be an integer");
      }
    }
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local activity database is unavailable");
    }
    try {
      return {true, database_->list_activities(server_id, limit)};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local activity database could not read activities");
    }
  }

  if (method == "__events.subscribe") return subscribe(params, true);
  if (method == "__events.unsubscribe") return subscribe(params, false);

  if (method == "shell.open_url") {
    if (!exact_string_fields(params, {"url"})) {
      return failure("INVALID_PARAMS", "shell.open_url requires exactly one URL");
    }
    const auto url = params.at("url").get<std::string>();
    if (!is_external_url(url)) {
      return failure("INVALID_URL", "Only http(s) and mailto links can be opened");
    }
    if (!open_external_url(url)) {
      return failure("OPEN_FAILED", "The system could not open the external URL");
    }
    return {true, {{"opened", true}}};
  }

  return failure("NOT_IMPLEMENTED",
                 "This desktop command has not yet migrated to the C++ host: " + method);
}

IpcReply IpcRouter::subscribe(const nlohmann::json& params, bool add) {
  if (!exact_string_fields(params, {"event", "token"})) {
    return failure("INVALID_PARAMS", "Event subscriptions require an event and token");
  }
  const auto event = params.at("event").get<std::string>();
  const auto token = params.at("token").get<std::string>();
  if (!valid_event_name(event) || !valid_subscription_token(token)) {
    return failure("INVALID_PARAMS", "The event subscription is invalid");
  }

  std::lock_guard lock(mutex_);
  if (add) {
    subscriptions_[event].insert(token);
    return {true, {{"subscribed", true}}};
  }
  if (auto found = subscriptions_.find(event); found != subscriptions_.end()) {
    found->second.erase(token);
    if (found->second.empty()) subscriptions_.erase(found);
  }
  return {true, {{"subscribed", false}}};
}

std::set<std::string> IpcRouter::subscribers(const std::string& event) const {
  std::lock_guard lock(mutex_);
  const auto found = subscriptions_.find(event);
  return found == subscriptions_.end() ? std::set<std::string>{} : found->second;
}

bool is_external_url(const std::string& url) {
  if (url.empty() || url.size() > 4096) return false;
  if (std::any_of(url.begin(), url.end(), [](unsigned char c) {
        return c <= 0x20 || c == 0x7f;
      })) {
    return false;
  }
  auto prefix = url.substr(0, std::min<size_t>(8, url.size()));
  std::transform(prefix.begin(), prefix.end(), prefix.begin(), [](unsigned char c) {
    return static_cast<char>(std::tolower(c));
  });
  if (prefix == "https://") return url.size() > 8 && url[8] != '/' && url[8] != '?' && url[8] != '#';
  if (url.size() >= 7) {
    auto http = url.substr(0, 7);
    std::transform(http.begin(), http.end(), http.begin(), [](unsigned char c) {
      return static_cast<char>(std::tolower(c));
    });
    if (http == "http://") return url.size() > 7 && url[7] != '/' && url[7] != '?' && url[7] != '#';
  }
  auto mailto = url.substr(0, std::min<size_t>(7, url.size()));
  std::transform(mailto.begin(), mailto.end(), mailto.begin(), [](unsigned char c) {
    return static_cast<char>(std::tolower(c));
  });
  return mailto == "mailto:" && url.size() > 7;
}

bool open_external_url(const std::string& url) {
  if (!is_external_url(url)) return false;
#ifdef _WIN32
  const auto wide_url = utf8_to_wide(url);
  if (wide_url.empty()) return false;
  const auto result = ShellExecuteW(nullptr, L"open", wide_url.c_str(), nullptr, nullptr,
                                    SW_SHOWNORMAL);
  return reinterpret_cast<INT_PTR>(result) > 32;
#else
  const char* executable =
#ifdef __APPLE__
      "open";
#else
      "xdg-open";
#endif
  char* const arguments[] = {const_cast<char*>(executable), const_cast<char*>(url.c_str()),
                             nullptr};
  pid_t process = 0;
  if (posix_spawnp(&process, executable, nullptr, nullptr, arguments, environ) != 0) return false;
  std::thread([process]() {
    int status = 0;
    while (waitpid(process, &status, 0) < 0 && errno == EINTR) {}
  }).detach();
  return true;
#endif
}

std::string operating_system() {
#ifdef _WIN32
  return "windows";
#elif defined(__APPLE__)
  return "macos";
#else
  return "linux";
#endif
}

}  // namespace yukinal::desktop_host

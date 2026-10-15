#include "yukinal/desktop_host/ipc_router.hpp"

#include <algorithm>
#include <atomic>
#include <cctype>
#include <chrono>
#include <cstdint>
#include <ctime>
#include <iomanip>
#include <initializer_list>
#include <regex>
#include <sstream>
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

bool is_one_of(const std::string& value, std::initializer_list<const char*> options) {
  return std::any_of(options.begin(), options.end(), [&value](const char* option) {
    return value == option;
  });
}

bool only_fields(const nlohmann::json& value, const std::set<std::string>& allowed,
                 const std::set<std::string>& required) {
  if (!value.is_object()) return false;
  for (const auto& field : required) {
    if (!value.contains(field)) return false;
  }
  for (auto item = value.begin(); item != value.end(); ++item) {
    if (!allowed.contains(item.key())) return false;
  }
  return true;
}

std::string trim_ascii(const std::string& value) {
  auto first = value.begin();
  while (first != value.end() && std::isspace(static_cast<unsigned char>(*first))) ++first;
  auto last = value.end();
  while (last != first && std::isspace(static_cast<unsigned char>(*(last - 1)))) --last;
  return std::string(first, last);
}

size_t utf8_character_count(const std::string& value) {
  return static_cast<size_t>(std::count_if(value.begin(), value.end(), [](unsigned char byte) {
    return (byte & 0xC0) != 0x80;
  }));
}

bool valid_opaque_id(const std::string& value) {
  if (value.empty() || utf8_character_count(value) > 256) return false;
  return std::all_of(value.begin(), value.end(), [](unsigned char byte) {
    return (byte >= 'a' && byte <= 'z') || (byte >= 'A' && byte <= 'Z') ||
           (byte >= '0' && byte <= '9') || byte == '_' || byte == '-' || byte == '.';
  });
}

bool has_control_characters(const std::string& value) {
  return std::any_of(value.begin(), value.end(), [](unsigned char byte) {
    return byte < 0x20 || byte == 0x7f;
  });
}

bool valid_chat_filename(const std::string& value, size_t max_characters,
                         bool allow_path_separators = false) {
  if (value.empty() || utf8_character_count(value) > max_characters ||
      value != trim_ascii(value) || has_control_characters(value)) return false;
  return allow_path_separators ||
         (value.find('/') == std::string::npos && value.find('\\') == std::string::npos);
}

bool valid_base64(const std::string& value, size_t max_decoded_bytes,
                  size_t& decoded_bytes) {
  decoded_bytes = 0;
  if (value.size() < 4 || value.size() % 4 != 0) return false;
  size_t padding = 0;
  if (!value.empty() && value.back() == '=') ++padding;
  if (value.size() > 1 && value[value.size() - 2] == '=') ++padding;
  for (size_t index = 0; index < value.size() - padding; ++index) {
    const auto byte = static_cast<unsigned char>(value[index]);
    if (!((byte >= 'a' && byte <= 'z') || (byte >= 'A' && byte <= 'Z') ||
          (byte >= '0' && byte <= '9') || byte == '+' || byte == '/')) return false;
  }
  for (size_t index = value.size() - padding; index < value.size(); ++index) {
    if (value[index] != '=') return false;
  }
  decoded_bytes = (value.size() / 4) * 3 - padding;
  return decoded_bytes <= max_decoded_bytes;
}

bool valid_chat_prompt_parts(const nlohmann::json& parts) {
  if (!parts.is_array() || parts.empty() || parts.size() > 25) return false;
  size_t text_characters = 0;
  size_t inline_bytes = 0;
  size_t file_bytes = 0;
  size_t images = 0;
  size_t documents = 0;
  size_t audios = 0;
  size_t files = 0;
  for (const auto& part : parts) {
    if (!part.is_object() || !part.contains("type") || !part.at("type").is_string()) return false;
    const auto type = part.at("type").get<std::string>();
    if (type == "text") {
      if (!only_fields(part, {"type", "text"}, {"type", "text"}) ||
          !part.at("text").is_string() || part.at("text").get<std::string>().empty()) return false;
      text_characters += utf8_character_count(part.at("text").get<std::string>());
      continue;
    }

    if (type == "image" || type == "document" || type == "audio" || type == "file") {
      const bool name_optional = type == "image" || type == "audio";
      const std::set<std::string> allowed = {"type", "mediaType", "data", "name"};
      const std::set<std::string> required = name_optional
          ? std::set<std::string>{"type", "mediaType", "data"}
          : std::set<std::string>{"type", "mediaType", "data", "name"};
      if (!only_fields(part, allowed, required) || !part.at("mediaType").is_string() ||
          !part.at("data").is_string()) return false;
      const auto media_type = part.at("mediaType").get<std::string>();
      const auto data = part.at("data").get<std::string>();
      if (part.contains("name") && !part.at("name").is_string()) return false;

      if (type == "file") {
        if (media_type != "text/plain" || data.empty() || data.size() > 512 * 1024 ||
            !part.at("name").is_string()) return false;
        if (!valid_chat_filename(part.at("name").get<std::string>(), 128)) return false;
        for (const auto byte : data) {
          const auto value = static_cast<unsigned char>(byte);
          if (value == 0 || (value < 0x20 && value != '\n' && value != '\r' && value != '\t') ||
              value == 0x7f) return false;
        }
        file_bytes += data.size();
        ++files;
        continue;
      }

      size_t decoded = 0;
      size_t max_bytes = 0;
      if (type == "image") {
        if (!is_one_of(media_type, {"image/png", "image/jpeg", "image/webp", "image/gif"})) return false;
        max_bytes = 5 * 1024 * 1024;
        ++images;
        if (part.contains("name") &&
            !valid_chat_filename(part.at("name").get<std::string>(), 128, true)) return false;
      } else if (type == "document") {
        if (media_type != "application/pdf" ||
            !valid_chat_filename(part.at("name").get<std::string>(), 128)) return false;
        max_bytes = 8 * 1024 * 1024;
        ++documents;
      } else {
        if (!is_one_of(media_type, {"audio/wav", "audio/mpeg", "audio/ogg", "audio/flac"})) return false;
        max_bytes = 8 * 1024 * 1024;
        ++audios;
        if (part.contains("name") &&
            !valid_chat_filename(part.at("name").get<std::string>(), 128)) return false;
      }
      if (!valid_base64(data, max_bytes, decoded)) return false;
      inline_bytes += decoded;
      if (inline_bytes > 12 * 1024 * 1024) return false;
      continue;
    }
    return false;
  }
  return text_characters <= 100'000 && inline_bytes <= 12 * 1024 * 1024 &&
         file_bytes <= 2 * 1024 * 1024 && images <= 8 && documents <= 4 &&
         audios <= 4 && files <= 8;
}

bool valid_request_text(const nlohmann::json& params, const char* key, size_t max_chars,
                        bool require_nonempty, std::string& result) {
  const auto& value = params.at(key);
  if (!value.is_string()) return false;
  result = trim_ascii(value.get<std::string>());
  const auto count = utf8_character_count(result);
  return (!require_nonempty || count > 0) && count <= max_chars;
}

bool valid_integer_range(const nlohmann::json& value, int minimum, int maximum, int& result) {
  if (value.is_number_unsigned()) {
    const auto requested = value.get<std::uint64_t>();
    if (requested < static_cast<std::uint64_t>(minimum) ||
        requested > static_cast<std::uint64_t>(maximum)) return false;
    result = static_cast<int>(requested);
    return true;
  }
  if (!value.is_number_integer()) return false;
  const auto requested = value.get<std::int64_t>();
  if (requested < minimum || requested > maximum) return false;
  result = static_cast<int>(requested);
  return true;
}

std::string iso8601_now() {
  const auto now = std::chrono::system_clock::now();
  const auto time = std::chrono::system_clock::to_time_t(now);
  std::tm utc{};
#ifdef _WIN32
  gmtime_s(&utc, &time);
#else
  gmtime_r(&time, &utc);
#endif
  std::ostringstream rendered;
  rendered << std::put_time(&utc, "%Y-%m-%dT%H:%M:%SZ");
  return rendered.str();
}

std::string next_id(const char* prefix) {
  static std::atomic<std::uint64_t> counter{1};
  const auto millis = std::chrono::duration_cast<std::chrono::milliseconds>(
                          std::chrono::system_clock::now().time_since_epoch())
                          .count();
  std::ostringstream rendered;
  rendered << prefix << '_' << std::hex << static_cast<std::uint64_t>(millis)
           << counter.fetch_add(1, std::memory_order_relaxed);
  return rendered.str();
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

  if (method == "tool_execution_list") {
    for (auto item = params.begin(); item != params.end(); ++item) {
      if (item.key() != "traceId" && item.key() != "serverId" && item.key() != "limit") {
        return failure("INVALID_PARAMS", "tool_execution_list received an unexpected parameter");
      }
    }
    std::optional<std::string> trace_id;
    if (params.contains("traceId")) {
      if (!params.at("traceId").is_string()) {
        return failure("INVALID_PARAMS", "tool_execution_list traceId must be a string");
      }
      const auto value = params.at("traceId").get<std::string>();
      if (value.empty()) return failure("INVALID_PARAMS", "tool_execution_list traceId is empty");
      trace_id = value;
    }
    std::optional<std::string> server_id;
    if (params.contains("serverId")) {
      if (!params.at("serverId").is_string()) {
        return failure("INVALID_PARAMS", "tool_execution_list serverId must be a string");
      }
      const auto value = params.at("serverId").get<std::string>();
      if (!valid_server_id(value)) {
        return failure("INVALID_PARAMS", "tool_execution_list serverId is invalid");
      }
      server_id = value;
    }
    int limit = 50;
    if (params.contains("limit")) {
      const auto& value = params.at("limit");
      if (value.is_number_unsigned()) {
        const auto requested_limit = value.get<std::uint64_t>();
        if (requested_limit < 1 || requested_limit > 100) {
          return failure("INVALID_PARAMS", "tool_execution_list limit must be between 1 and 100");
        }
        limit = static_cast<int>(requested_limit);
      } else if (value.is_number_integer()) {
        const auto requested_limit = value.get<std::int64_t>();
        if (requested_limit < 1 || requested_limit > 100) {
          return failure("INVALID_PARAMS", "tool_execution_list limit must be between 1 and 100");
        }
        limit = static_cast<int>(requested_limit);
      } else {
        return failure("INVALID_PARAMS", "tool_execution_list limit must be an integer");
      }
    }
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local execution database is unavailable");
    }
    try {
      return {true, database_->list_tool_executions(trace_id, server_id, limit)};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local execution database could not read executions");
    }
  }

  if (method == "chat_session_list") {
    if (!only_fields(params, {"query", "archived", "offset", "limit"}, {})) {
      return failure("INVALID_PARAMS", "chat_session_list received unexpected parameters");
    }
    std::optional<std::string> query;
    if (params.contains("query")) {
      std::string value;
      if (!valid_request_text(params, "query", 200, false, value)) {
        return failure("INVALID_PARAMS", "chat_session_list query must be at most 200 characters");
      }
      query = std::move(value);
    }
    std::optional<bool> archived;
    if (params.contains("archived")) {
      if (!params.at("archived").is_boolean()) {
        return failure("INVALID_PARAMS", "chat_session_list archived must be a boolean");
      }
      archived = params.at("archived").get<bool>();
    }
    int limit = 50;
    if (params.contains("limit") && !valid_integer_range(params.at("limit"), 1, 100, limit)) {
      return failure("INVALID_PARAMS", "chat_session_list limit must be between 1 and 100");
    }
    int offset = 0;
    if (params.contains("offset") && !valid_integer_range(params.at("offset"), 0, 10'000, offset)) {
      return failure("INVALID_PARAMS", "chat_session_list offset must be between 0 and 10000");
    }
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local chat database is unavailable");
    }
    try {
      return {true, database_->list_chat_sessions(query, archived, limit, offset)};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local chat database could not list sessions");
    }
  }

  if (method == "chat_session_get") {
    if (!only_fields(params, {"sessionId"}, {"sessionId"})) {
      return failure("INVALID_PARAMS", "chat_session_get requires a sessionId");
    }
    std::string session_id;
    if (!valid_request_text(params, "sessionId", 256, true, session_id) ||
        !valid_opaque_id(session_id)) {
      return failure("INVALID_PARAMS", "chat_session_get sessionId is invalid");
    }
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local chat database is unavailable");
    }
    try {
      return {true, database_->get_chat_session(session_id)};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local chat database could not read the session");
    }
  }

  if (method == "chat_session_create") {
    if (!only_fields(params, {"sessionId", "workspaceId", "serverId", "title"}, {"title"})) {
      return failure("INVALID_PARAMS", "chat_session_create received invalid parameters");
    }
    std::string title;
    if (!valid_request_text(params, "title", 200, true, title)) {
      return failure("INVALID_PARAMS", "chat_session_create title must be 1 to 200 characters");
    }
    std::optional<std::string> session_id;
    if (params.contains("sessionId")) {
      std::string value;
      if (!valid_request_text(params, "sessionId", 256, true, value) || !valid_opaque_id(value)) {
        return failure("INVALID_PARAMS", "chat_session_create sessionId is invalid");
      }
      session_id = std::move(value);
    }
    std::optional<std::string> workspace_id;
    if (params.contains("workspaceId")) {
      std::string value;
      if (!valid_request_text(params, "workspaceId", 256, true, value)) {
        return failure("INVALID_PARAMS", "chat_session_create workspaceId is invalid");
      }
      workspace_id = std::move(value);
    }
    std::optional<std::string> server_id;
    if (params.contains("serverId")) {
      if (!params.at("serverId").is_string()) {
        return failure("INVALID_PARAMS", "chat_session_create serverId must be a string");
      }
      auto value = trim_ascii(params.at("serverId").get<std::string>());
      if (!valid_server_id(value)) {
        return failure("INVALID_PARAMS", "chat_session_create serverId is invalid");
      }
      server_id = std::move(value);
    }
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local chat database is unavailable");
    }
    try {
      return {true, database_->create_chat_session(
                        session_id.value_or(next_id("ses")), workspace_id, server_id,
                        title, iso8601_now())};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local chat database could not create the session");
    }
  }

  if (method == "chat_message_append") {
    if (!only_fields(params,
                     {"sessionId", "messageId", "role", "content", "parts", "traceId", "createdAt"},
                     {"sessionId", "role", "content"})) {
      return failure("INVALID_PARAMS", "chat_message_append received invalid parameters");
    }
    std::string session_id;
    if (!valid_request_text(params, "sessionId", 256, true, session_id) ||
        !valid_opaque_id(session_id)) {
      return failure("INVALID_PARAMS", "chat_message_append sessionId is invalid");
    }
    std::string role;
    if (!valid_request_text(params, "role", 16, true, role) ||
        !is_one_of(role, {"user", "assistant", "tool", "system"})) {
      return failure("INVALID_PARAMS", "chat_message_append role is invalid");
    }
    if (!params.at("content").is_string()) {
      return failure("INVALID_PARAMS", "chat_message_append content must be a string");
    }
    auto content = trim_ascii(params.at("content").get<std::string>());
    if (utf8_character_count(content) > 100'000) {
      return failure("INVALID_PARAMS", "chat_message_append content is too long");
    }
    std::optional<nlohmann::json> parts;
    if (params.contains("parts")) {
      if (!valid_chat_prompt_parts(params.at("parts"))) {
        return failure("INVALID_PARAMS", "chat_message_append prompt parts are invalid");
      }
      parts = params.at("parts");
    }
    if (content.empty() && !parts.has_value()) {
      return failure("INVALID_PARAMS", "chat_message_append requires content or prompt parts");
    }
    std::optional<std::string> message_id;
    if (params.contains("messageId")) {
      std::string value;
      if (!valid_request_text(params, "messageId", 256, true, value) || !valid_opaque_id(value)) {
        return failure("INVALID_PARAMS", "chat_message_append messageId is invalid");
      }
      message_id = std::move(value);
    }
    std::optional<std::string> trace_id;
    if (params.contains("traceId")) {
      std::string value;
      if (!valid_request_text(params, "traceId", 256, true, value)) {
        return failure("INVALID_PARAMS", "chat_message_append traceId is invalid");
      }
      trace_id = std::move(value);
    }
    std::string timestamp = iso8601_now();
    if (params.contains("createdAt") &&
        !valid_request_text(params, "createdAt", 80, true, timestamp)) {
      return failure("INVALID_PARAMS", "chat_message_append createdAt is invalid");
    }
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local chat database is unavailable");
    }
    try {
      return {true, database_->append_chat_message(
                        message_id.value_or(next_id("msg")), session_id, role, content,
                        parts, trace_id, timestamp)};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local chat database could not append the message");
    }
  }

  if (method == "chat_session_archive") {
    if (!only_fields(params, {"sessionId", "archived"}, {"sessionId", "archived"}) ||
        !params.at("archived").is_boolean()) {
      return failure("INVALID_PARAMS", "chat_session_archive requires sessionId and archived");
    }
    std::string session_id;
    if (!valid_request_text(params, "sessionId", 256, true, session_id) ||
        !valid_opaque_id(session_id)) {
      return failure("INVALID_PARAMS", "chat_session_archive sessionId is invalid");
    }
    const auto archived = params.at("archived").get<bool>();
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local chat database is unavailable");
    }
    try {
      return {true, database_->set_chat_session_archived(
                        session_id, archived ? std::optional<std::string>(iso8601_now())
                                             : std::nullopt)};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local chat database could not update the session");
    }
  }

  if (method == "chat_session_rename") {
    if (!only_fields(params, {"sessionId", "title"}, {"sessionId", "title"})) {
      return failure("INVALID_PARAMS", "chat_session_rename requires sessionId and title");
    }
    std::string session_id;
    std::string title;
    if (!valid_request_text(params, "sessionId", 256, true, session_id) ||
        !valid_opaque_id(session_id) || !valid_request_text(params, "title", 200, true, title)) {
      return failure("INVALID_PARAMS", "chat_session_rename parameters are invalid");
    }
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local chat database is unavailable");
    }
    try {
      return {true, database_->rename_chat_session(session_id, title)};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local chat database could not rename the session");
    }
  }

  if (method == "chat_session_delete") {
    if (!only_fields(params, {"sessionId"}, {"sessionId"})) {
      return failure("INVALID_PARAMS", "chat_session_delete requires sessionId");
    }
    std::string session_id;
    if (!valid_request_text(params, "sessionId", 256, true, session_id) ||
        !valid_opaque_id(session_id)) {
      return failure("INVALID_PARAMS", "chat_session_delete sessionId is invalid");
    }
    if (database_ == nullptr) {
      return failure("STORAGE_UNAVAILABLE", "The local chat database is unavailable");
    }
    try {
      return {true, database_->delete_chat_session(session_id)};
    } catch (...) {
      return failure("STORAGE_ERROR", "The local chat database could not delete the session");
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

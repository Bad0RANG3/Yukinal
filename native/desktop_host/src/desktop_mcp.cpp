#include "yukinal/desktop_host/desktop_mcp.hpp"

#include "yukinal/desktop_host/credential_store.hpp"
#include "yukinal/desktop_host/database.hpp"
#include "yukinal/desktop_host/desktop_oauth.hpp"
#include "header_safety.hpp"

#include <httplib.h>

#include <algorithm>
#include <atomic>
#include <cctype>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <cstdlib>
#include <ctime>
#include <iomanip>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <random>
#include <sstream>
#include <stdexcept>
#include <thread>
#include <utility>
#include <vector>

#ifdef _WIN32
#include <windows.h>
#else
#include <cerrno>
#include <fcntl.h>
#include <signal.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>
extern char** environ;
#endif

namespace yukinal::desktop_host {
namespace {

constexpr std::size_t kMaxFrameBytes = 8 * 1024 * 1024;
constexpr std::size_t kMaxStderrLines = 40;
constexpr const char* kProtocolVersion = "2024-11-05";
constexpr const char* kClientName = "yukinal-desktop";
constexpr const char* kClientVersion = "2.0.0-rc.5";

std::string sanitize_segment(const std::string& value) {
  std::string out;
  out.reserve(value.size());
  for (const char character : value) {
    if (std::isalnum(static_cast<unsigned char>(character)) != 0) {
      out.push_back(character);
    } else {
      out.push_back('_');
    }
  }
  if (out.empty()) out = "x";
  return out;
}

std::string internal_tool_name(const std::string& server_segment, const std::string& tool) {
  return "mcp." + server_segment + "." + sanitize_segment(tool);
}

// The shared `McpServerStatus.startedAt` shape: UTC seconds, no fractional part.
std::string iso8601_utc_now() {
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

bool split_internal_name(const std::string& name, std::string& server_segment,
                         std::string& tool_segment) {
  if (name.rfind("mcp.", 0) != 0) return false;
  const auto rest = name.substr(4);
  const auto dot = rest.find('.');
  if (dot == std::string::npos || dot == 0 || dot + 1 >= rest.size()) return false;
  if (rest.find('.', dot + 1) != std::string::npos) return false;
  server_segment = rest.substr(0, dot);
  tool_segment = rest.substr(dot + 1);
  return true;
}

std::string effective_risk(const std::string& annotation_trust, const nlohmann::json& annotations) {
  if (annotation_trust != "trusted") return "critical";
  if (annotations.is_object() && annotations.value("readOnlyHint", false)) return "low";
  if (annotations.is_object() && annotations.contains("destructiveHint") &&
      annotations.at("destructiveHint").is_boolean() &&
      annotations.at("destructiveHint").get<bool>() == false) {
    return "medium";
  }
  return "high";
}

// One Streamable HTTP endpoint split into the pieces httplib wants: an
// origin (`scheme://host:port`) and the path to POST JSON-RPC frames to.
struct HttpUrlParts {
  std::string origin;
  std::string path;
};

bool parse_http_url(const std::string& url, HttpUrlParts& out) {
  const bool secure = url.rfind("https://", 0) == 0;
  const bool plain = url.rfind("http://", 0) == 0;
  if (!secure && !plain) return false;
  const std::size_t base = secure ? 8 : 7;
  const auto slash = url.find('/', base);
  const std::string authority =
      slash == std::string::npos ? url.substr(base) : url.substr(base, slash - base);
  const std::string path = slash == std::string::npos ? std::string("/") : url.substr(slash);
  if (authority.empty()) return false;
  std::string host;
  std::string port;
  if (authority.front() == '[') {
    const auto close = authority.find(']');
    if (close == std::string::npos) return false;
    host = authority.substr(0, close + 1);
    if (close + 1 < authority.size()) {
      if (authority.at(close + 1) != ':') return false;
      port = authority.substr(close + 2);
    }
  } else {
    const auto colon = authority.find(':');
    if (colon == std::string::npos) {
      host = authority;
    } else {
      host = authority.substr(0, colon);
      port = authority.substr(colon + 1);
    }
  }
  if (host.empty()) return false;
  if (port.empty()) port = secure ? "443" : "80";
  out.origin = (secure ? "https://" : "http://") + host + ":" + port;
  out.path = path;
  return true;
}

// Lower-case, trimmed media type without its parameters.
std::string content_type_kind(std::string value) {
  const auto semicolon = value.find(';');
  if (semicolon != std::string::npos) value.resize(semicolon);
  while (!value.empty() && std::isspace(static_cast<unsigned char>(value.front())) != 0) {
    value.erase(value.begin());
  }
  while (!value.empty() && std::isspace(static_cast<unsigned char>(value.back())) != 0) {
    value.pop_back();
  }
  std::transform(value.begin(), value.end(), value.begin(), [](unsigned char byte) {
    return static_cast<char>(std::tolower(byte));
  });
  return value;
}

std::string truncate_text(std::string value, std::size_t limit) {
  if (value.size() > limit) {
    value.resize(limit);
    value += "...";
  }
  return value;
}

// How many times one request may be sent while authenticating: as-is, with the
// DPoP nonce the server just challenged with, then with a forcibly refreshed
// token (RFC 9449 §8). Mirrors the retired Rust transport.
constexpr int kAuthAttempts = 3;

std::string trim_ascii(std::string value) {
  std::size_t start = 0;
  while (start < value.size() && std::isspace(static_cast<unsigned char>(value[start])) != 0) {
    ++start;
  }
  std::size_t end = value.size();
  while (end > start && std::isspace(static_cast<unsigned char>(value[end - 1])) != 0) {
    --end;
  }
  return value.substr(start, end - start);
}

// One `auth-param` out of a `WWW-Authenticate` value (RFC 7235 §2.1).
std::string challenge_parameter(const std::string& header, const char* name) {
  std::string lowered = header;
  std::transform(lowered.begin(), lowered.end(), lowered.begin(), [](unsigned char byte) {
    return static_cast<char>(std::tolower(byte));
  });
  const std::string needle = std::string(name) + "=";
  std::size_t from = 0;
  while (true) {
    const auto offset = lowered.find(needle, from);
    if (offset == std::string::npos) return std::string();
    from = offset + needle.size();
    // A parameter name must start at a boundary: `xnonce=` is not `nonce=`.
    if (offset > 0) {
      const auto previous = static_cast<unsigned char>(header[offset - 1]);
      if (std::isalnum(previous) != 0 || previous == '-' || previous == '_') continue;
    }
    auto rest = header.substr(from);
    const auto first = rest.find_first_not_of(" \t");
    if (first == std::string::npos) return std::string();
    rest.erase(0, first);
    if (!rest.empty() && rest.front() == '"') {
      const auto end = rest.find('"', 1);
      if (end == std::string::npos) continue;
      return rest.substr(1, end - 1);
    }
    const auto end = rest.find_first_of(", \t");
    return rest.substr(0, end == std::string::npos ? rest.size() : end);
  }
}

// Decodes the `data:` payloads of one SSE body the way the Rust transport's
// decoder did: comments ignored, `\r` stripped, multi-line data joined with
// newlines, oversized events dropped.
std::vector<std::string> decode_sse(const std::string& body) {
  std::vector<std::string> events;
  std::string data;
  bool discarding = false;
  const auto finish_line = [&](std::string line) {
    if (!line.empty() && line.back() == '\r') line.pop_back();
    if (line.empty()) {
      if (!data.empty()) {
        events.push_back(data);
        data.clear();
      }
      discarding = false;
      return;
    }
    if (discarding || line.front() == ':') return;
    const auto colon = line.find(':');
    if (colon == std::string::npos) return;
    if (line.compare(0, colon, "data") != 0) return;
    std::string value = line.substr(colon + 1);
    if (!value.empty() && value.front() == ' ') value.erase(value.begin());
    if (data.size() + value.size() + 1 > kMaxFrameBytes) {
      data.clear();
      discarding = true;
      return;
    }
    if (!data.empty()) data.push_back('\n');
    data += value;
  };
  std::size_t start = 0;
  for (std::size_t index = 0; index <= body.size(); ++index) {
    if (index == body.size() || body.at(index) == '\n') {
      finish_line(body.substr(start, index - start));
      start = index + 1;
      if (index == body.size()) break;
    }
  }
  if (!discarding && !data.empty()) events.push_back(data);
  return events;
}

// Raised when a tool call observes its cancellation token.
class McpCallCancelled : public std::runtime_error {
 public:
  McpCallCancelled() : std::runtime_error("the MCP tool call was cancelled") {}
};

// Finds the JSON-RPC response frame for `id` in one payload (or a batch),
// returning whether it was found and, if so, its result or error text.
bool take_json_response(const nlohmann::json& payload, long long id,
                        nlohmann::json& result, std::string& error_message) {
  if (payload.is_array()) {
    for (const auto& frame : payload) {
      if (take_json_response(frame, id, result, error_message)) return true;
    }
    return false;
  }
  if (!payload.is_object() || !payload.contains("id") ||
      !payload.at("id").is_number_integer() || payload.at("id").get<long long>() != id) {
    return false;
  }
  if (payload.contains("error")) {
    const auto& error = payload.at("error");
    error_message = error.is_object() && error.contains("message") &&
                            error.at("message").is_string()
                        ? error.at("message").get<std::string>()
                        : std::string("the MCP server rejected the request");
    return true;
  }
  result = payload.value("result", nlohmann::json());
  return true;
}

}  // namespace

struct McpSupervisor::Impl {
  struct Pending {
    std::mutex mutex;
    std::condition_variable cv;
    bool done = false;
    bool has_error = false;
    std::string error_message;
    nlohmann::json result;
  };

  struct Session {
    std::string server_id;
    std::string segment;
    std::string label;
    std::string annotation_trust;
    nlohmann::json allowed_tools = nlohmann::json::array();
    std::mutex mutex;
    std::mutex io_mutex;
    long long next_id = 1;
    std::map<long long, std::shared_ptr<Pending>> pending;
    bool running = false;
    bool exited = false;
    std::string protocol_version;
    std::string server_name;
    std::string server_version;
    nlohmann::json tools = nlohmann::json::array();
    std::vector<std::string> stderr_tail;
    // "stdio" (a child process) or "http" (one Streamable HTTP endpoint). The
    // process fields below are unused for HTTP sessions.
    std::string transport = "stdio";
    std::string http_origin;
    std::string http_path;
    std::vector<std::pair<std::string, std::string>> http_headers;
    std::string http_session_id;
    std::vector<std::string> diagnostics;
    // What the read-only status reports. A stdio session records the launched
    // program and its process id; an HTTP session has no local process, so both
    // stay empty and the status renders them as null.
    std::string program;
    long long process_id = 0;
    std::string started_at;
    // Present when the server row carries a connected OAuth configuration. It
    // resolves the stored token (refreshing when needed) and signs a per-request
    // DPoP proof; the plaintext token never leaves this object.
    std::unique_ptr<OAuthTokenSource> oauth;
#ifdef _WIN32
    HANDLE process = nullptr;
    HANDLE stdin_write = nullptr;
    HANDLE stdout_read = nullptr;
    HANDLE stderr_read = nullptr;
#else
    pid_t pid = -1;
    int stdin_write = -1;
    int stdout_read = -1;
    int stderr_read = -1;
#endif
    std::thread reader;
    std::thread stderr_reader;
  };

  DesktopDatabase* database = nullptr;
  const CredentialStore* credentials = nullptr;
  std::mutex mutex;
  std::map<std::string, std::shared_ptr<Session>> sessions;

  ~Impl() {
    std::map<std::string, std::shared_ptr<Session>> snapshot;
    {
      std::lock_guard lock(mutex);
      snapshot.swap(sessions);
    }
    for (auto& [id, session] : snapshot) shutdown(session);
  }

  void notify_pending(const std::shared_ptr<Session>& session) {
    std::map<long long, std::shared_ptr<Pending>> outstanding;
    {
      std::lock_guard lock(session->mutex);
      outstanding.swap(session->pending);
    }
    for (auto& [id, pending] : outstanding) {
      std::lock_guard lock(pending->mutex);
      pending->has_error = true;
      pending->error_message = "the MCP server exited before answering";
      pending->done = true;
      pending->cv.notify_all();
    }
  }

  void process_line(const std::shared_ptr<Session>& session, const std::string& line) {
    if (line.empty() || line.size() > kMaxFrameBytes) return;
    nlohmann::json frame;
    try {
      frame = nlohmann::json::parse(line);
    } catch (const nlohmann::json::exception&) {
      return;
    }
    if (!frame.is_object() || !frame.contains("id") || !frame.at("id").is_number_integer()) {
      return;
    }
    const auto id = frame.at("id").get<long long>();
    std::shared_ptr<Pending> pending;
    {
      std::lock_guard lock(session->mutex);
      const auto found = session->pending.find(id);
      if (found == session->pending.end()) return;
      pending = found->second;
      session->pending.erase(found);
    }
    std::lock_guard lock(pending->mutex);
    if (frame.contains("error")) {
      pending->has_error = true;
      const auto& error = frame.at("error");
      pending->error_message = error.is_object() && error.contains("message") &&
                                       error.at("message").is_string()
                                   ? error.at("message").get<std::string>()
                                   : "the MCP server rejected the request";
    } else if (frame.contains("result")) {
      pending->result = frame.at("result");
    } else {
      pending->has_error = true;
      pending->error_message = "the MCP server returned no result";
    }
    pending->done = true;
    pending->cv.notify_all();
  }

  void reader_loop(const std::shared_ptr<Session>& session) {
    std::string buffer;
    std::string chunk;
    while (read_stream(session, true, chunk)) {
      buffer += chunk;
      chunk.clear();
      std::size_t start = 0;
      for (;;) {
        const auto newline = buffer.find('\n', start);
        if (newline == std::string::npos) break;
        auto line = buffer.substr(start, newline - start);
        if (!line.empty() && line.back() == '\r') line.pop_back();
        process_line(session, line);
        start = newline + 1;
      }
      if (start > 0) buffer.erase(0, start);
      if (buffer.size() > kMaxFrameBytes) buffer.clear();
    }
    if (!buffer.empty()) process_line(session, buffer);
    {
      std::lock_guard lock(session->mutex);
      session->running = false;
      session->exited = true;
    }
    notify_pending(session);
  }

  void stderr_loop(const std::shared_ptr<Session>& session) {
    std::string chunk;
    std::string partial;
    while (read_stream(session, false, chunk)) {
      partial += chunk;
      chunk.clear();
      std::size_t start = 0;
      for (;;) {
        const auto newline = partial.find('\n', start);
        if (newline == std::string::npos) break;
        auto line = partial.substr(start, newline - start);
        if (!line.empty() && line.back() == '\r') line.pop_back();
        if (!line.empty()) {
          std::lock_guard lock(session->mutex);
          session->stderr_tail.push_back(line.size() > 4000 ? line.substr(0, 4000) : line);
          while (session->stderr_tail.size() > kMaxStderrLines) {
            session->stderr_tail.erase(session->stderr_tail.begin());
          }
        }
        start = newline + 1;
      }
      if (start > 0) partial.erase(0, start);
    }
  }

  bool read_stream(const std::shared_ptr<Session>& session, bool stdout_stream,
                   std::string& chunk) {
    chunk.clear();
#ifdef _WIN32
    HANDLE handle = nullptr;
    {
      std::lock_guard lock(session->io_mutex);
      handle = stdout_stream ? session->stdout_read : session->stderr_read;
    }
    if (handle == nullptr) return false;
    char buffer[16 * 1024];
    DWORD read = 0;
    if (!ReadFile(handle, buffer, sizeof(buffer), &read, nullptr) || read == 0) return false;
    chunk.assign(buffer, buffer + read);
    return true;
#else
    int descriptor = -1;
    {
      std::lock_guard lock(session->io_mutex);
      descriptor = stdout_stream ? session->stdout_read : session->stderr_read;
    }
    if (descriptor < 0) return false;
    char buffer[16 * 1024];
    const auto read = ::read(descriptor, buffer, sizeof(buffer));
    if (read <= 0) return false;
    chunk.assign(buffer, buffer + read);
    return true;
#endif
  }

  bool write_line(const std::shared_ptr<Session>& session, const nlohmann::json& frame) {
    const auto payload = frame.dump() + "\n";
    std::lock_guard lock(session->io_mutex);
#ifdef _WIN32
    if (session->stdin_write == nullptr) return false;
    DWORD written = 0;
    std::size_t offset = 0;
    while (offset < payload.size()) {
      const auto remaining = static_cast<DWORD>(
          std::min<std::size_t>(payload.size() - offset, 1 << 20));
      if (!WriteFile(session->stdin_write, payload.data() + offset, remaining, &written, nullptr) ||
          written == 0) {
        return false;
      }
      offset += written;
    }
    return true;
#else
    if (session->stdin_write < 0) return false;
    std::size_t offset = 0;
    while (offset < payload.size()) {
      const auto written = ::write(session->stdin_write, payload.data() + offset,
                                   payload.size() - offset);
      if (written < 0) {
        if (errno == EINTR) continue;
        return false;
      }
      offset += static_cast<std::size_t>(written);
    }
    return true;
#endif
  }

  // Applies the static auth headers plus the negotiated session and protocol
  // headers to one HTTP request.
  httplib::Headers http_headers_for(const std::shared_ptr<Session>& session) const {
    httplib::Headers headers = {{"Accept", "application/json, text/event-stream"}};
    for (const auto& [name, value] : session->http_headers) headers.emplace(name, value);
    std::string session_id;
    std::string protocol_version;
    {
      std::lock_guard lock(session->mutex);
      session_id = session->http_session_id;
      protocol_version = session->protocol_version;
    }
    if (!session_id.empty()) headers.emplace("Mcp-Session-Id", session_id);
    if (!protocol_version.empty()) headers.emplace("Mcp-Protocol-Version", protocol_version);
    return headers;
  }

  // The nonce a server challenged this request with, or an empty string. Two
  // spellings mean the same thing: the `DPoP-Nonce` header (RFC 9449 §8) and a
  // `nonce="..."` parameter on a `WWW-Authenticate: DPoP` challenge. The value
  // comes straight off the network, so it is length- and control-character-
  // checked here; anything that fails counts as "no challenge".
  static std::string dpop_nonce_from_response(const httplib::Response& response) {
    std::string value = response.get_header_value("DPoP-Nonce");
    if (value.empty()) {
      const auto challenge = response.get_header_value("WWW-Authenticate");
      auto lowered = challenge;
      std::transform(lowered.begin(), lowered.end(), lowered.begin(), [](unsigned char byte) {
        return static_cast<char>(std::tolower(byte));
      });
      const auto first = lowered.find_first_not_of(" \t");
      if (first != std::string::npos) lowered.erase(0, first);
      if (lowered.rfind("dpop", 0) != 0) return std::string();
      value = challenge_parameter(challenge, "nonce");
    }
    value = trim_ascii(std::move(value));
    if (value.empty() || value.size() > 1024 || has_header_control_characters(value)) return std::string();
    return value;
  }

  // The static headers plus, when the server is OAuth-connected, the resolved
  // `Authorization` (and per-request `DPoP` proof). The token and proof come
  // from the credential store, so they never leave this function.
  httplib::Headers http_auth_headers(const std::shared_ptr<Session>& session,
                                     const std::string& method, const std::string& url,
                                     bool force_refresh, const std::string& nonce) const {
    httplib::Headers headers = http_headers_for(session);
    if (session->oauth == nullptr) return headers;
    const auto authorization = session->oauth->authorize(method, url, nonce, force_refresh);
    const auto safe = [](const std::string& value) {
      return !value.empty() && value.size() <= 8 * 1024 &&
             value.find_first_of("\r\n") == std::string::npos &&
             value.find('\0') == std::string::npos;
    };
    if (!safe(authorization.token)) {
      throw std::runtime_error(
          "the OAuth token source returned an empty, oversized, or unsafe token");
    }
    if (authorization.scheme == "DPoP") {
      if (!safe(authorization.proof)) {
        throw std::runtime_error("the OAuth token source returned an unsafe DPoP proof");
      }
      headers.emplace("Authorization", "DPoP " + authorization.token);
      headers.emplace("DPoP", authorization.proof);
    } else {
      headers.emplace("Authorization", "Bearer " + authorization.token);
    }
    return headers;
  }

  // Sends one request, retrying a `401` a bounded number of times: as-is, then
  // with the DPoP nonce the server just challenged with, then with a forcibly
  // refreshed token. The last response is handed back when none of them worked,
  // so the caller still reports the server's own status.
  template <typename Send>
  httplib::Result send_with_auth(const std::shared_ptr<Session>& session,
                                 const std::string& method, const std::string& url,
                                 Send&& send) const {
    httplib::Result response;
    bool force_refresh = false;
    std::string nonce;
    for (int attempt = 0; attempt < kAuthAttempts; ++attempt) {
      auto headers = http_auth_headers(session, method, url, force_refresh, nonce);
      response = send(headers);
      if (!response || response->status != 401 || session->oauth == nullptr) break;
      if (attempt + 1 >= kAuthAttempts) break;
      const auto challenge = dpop_nonce_from_response(*response);
      if (!challenge.empty() && challenge != nonce) {
        nonce = challenge;
        continue;
      }
      if (!force_refresh) {
        force_refresh = true;
        continue;
      }
      break;
    }
    return response;
  }

  nlohmann::json http_send(const std::shared_ptr<Session>& session, const nlohmann::json& frame,
                           std::optional<long long> wait_for, int timeout_seconds) {
    httplib::Client client(session->http_origin);
    client.set_connection_timeout(10, 0);
    client.set_read_timeout(timeout_seconds, 0);
    client.set_write_timeout(timeout_seconds, 0);
    client.set_follow_location(false);
    const auto body = frame.dump();
    if (body.size() > kMaxFrameBytes) {
      throw std::runtime_error("the MCP HTTP frame exceeds the size limit");
    }
    const auto absolute_url = session->http_origin + session->http_path;
    auto response =
        send_with_auth(session, "POST", absolute_url, [&](const httplib::Headers& headers) {
          return client.Post(session->http_path, headers, body, "application/json");
        });
    if (!response) {
      throw std::runtime_error("the MCP HTTP request failed: " +
                               httplib::to_string(response.error()));
    }
    const auto returned_session = response->get_header_value("Mcp-Session-Id");
    if (!returned_session.empty()) {
      if (returned_session.size() > 1024 ||
          returned_session.find_first_of("\r\n") != std::string::npos) {
        throw std::runtime_error("the MCP server returned an invalid Mcp-Session-Id");
      }
      std::lock_guard lock(session->mutex);
      session->http_session_id = returned_session;
    }
    const auto status = response->status;
    if (status == 404 && !session->http_session_id.empty()) {
      {
        std::lock_guard lock(session->mutex);
        session->running = false;
      }
      throw std::runtime_error("HTTP 404: the MCP session is no longer known");
    }
    if (status < 200 || status >= 300) {
      throw std::runtime_error("HTTP " + std::to_string(status) + ": " +
                               truncate_text(response->body, 2000));
    }
    if (status == 202 || status == 204) return nlohmann::json();
    const auto kind = content_type_kind(response->get_header_value("Content-Type"));
    if (kind.empty() && !wait_for.has_value()) return nlohmann::json();
    if (kind == "text/event-stream" || (kind.empty() && wait_for.has_value())) {
      if (!wait_for.has_value()) return nlohmann::json();
      for (const auto& event : decode_sse(response->body)) {
        auto parsed = nlohmann::json::parse(event, nullptr, false);
        if (parsed.is_discarded()) continue;
        nlohmann::json result;
        std::string error_message;
        if (take_json_response(parsed, *wait_for, result, error_message)) {
          if (!error_message.empty()) throw std::runtime_error(error_message);
          return result;
        }
      }
      throw std::runtime_error("the MCP server did not answer over the event stream");
    }
    if (kind == "application/json") {
      auto parsed = nlohmann::json::parse(response->body, nullptr, false);
      if (parsed.is_discarded()) {
        throw std::runtime_error("the MCP server returned invalid JSON");
      }
      if (!wait_for.has_value()) return nlohmann::json();
      nlohmann::json result;
      std::string error_message;
      if (!take_json_response(parsed, *wait_for, result, error_message)) {
        throw std::runtime_error("the MCP server answered an unexpected request id");
      }
      if (!error_message.empty()) throw std::runtime_error(error_message);
      return result;
    }
    throw std::runtime_error("the MCP HTTP response has unsupported content type \"" + kind +
                             "\"");
  }

  nlohmann::json http_request(const std::shared_ptr<Session>& session, const std::string& method,
                              const nlohmann::json& params, int timeout_seconds,
                              const std::shared_ptr<std::atomic<bool>>& cancel = nullptr) {
    if (cancel != nullptr && cancel->load()) throw McpCallCancelled();
    long long id = 0;
    {
      std::lock_guard lock(session->mutex);
      if (!session->running) throw std::runtime_error("the MCP server is not running");
      id = session->next_id++;
    }
    const auto result = http_send(
        session, {{"jsonrpc", "2.0"}, {"id", id}, {"method", method}, {"params", params}}, id,
        timeout_seconds);
    // The blocking POST cannot be interrupted mid-flight; a cancellation that
    // arrives during it is honoured on return.
    if (cancel != nullptr && cancel->load()) throw McpCallCancelled();
    return result;
  }

  void delete_http_session(const std::shared_ptr<Session>& session) {
    std::string session_id;
    {
      std::lock_guard lock(session->mutex);
      session_id = session->http_session_id;
    }
    if (session_id.empty()) return;
    httplib::Client client(session->http_origin);
    client.set_connection_timeout(10, 0);
    client.set_read_timeout(2, 0);
    client.set_write_timeout(2, 0);
    client.set_follow_location(false);
    const auto absolute_url = session->http_origin + session->http_path;
    auto response =
        send_with_auth(session, "DELETE", absolute_url, [&](const httplib::Headers& headers) {
          return client.Delete(session->http_path, headers);
        });
    if (!response) return;
    const auto status = response->status;
    if ((status >= 200 && status < 300) || status == 405 || status == 404) {
      std::lock_guard lock(session->mutex);
      session->http_session_id.clear();
    }
  }

  nlohmann::json request(const std::shared_ptr<Session>& session, const std::string& method,
                         const nlohmann::json& params, int timeout_seconds,
                         const std::shared_ptr<std::atomic<bool>>& cancel = nullptr) {
    if (session->transport == "http") {
      return http_request(session, method, params, timeout_seconds, cancel);
    }
    auto pending = std::make_shared<Pending>();
    long long id = 0;
    {
      std::lock_guard lock(session->mutex);
      if (!session->running) throw std::runtime_error("the MCP server is not running");
      id = session->next_id++;
      session->pending[id] = pending;
    }
    if (!write_line(session, {{"jsonrpc", "2.0"}, {"id", id}, {"method", method}, {"params", params}})) {
      std::lock_guard lock(session->mutex);
      session->pending.erase(id);
      throw std::runtime_error("could not write to the MCP server");
    }
    std::unique_lock lock(pending->mutex);
    const auto deadline =
        std::chrono::steady_clock::now() + std::chrono::seconds(timeout_seconds);
    for (;;) {
      if (cancel != nullptr && cancel->load()) {
        lock.unlock();
        {
          std::lock_guard state_lock(session->mutex);
          session->pending.erase(id);
        }
        throw McpCallCancelled();
      }
      if (pending->cv.wait_for(lock, std::chrono::milliseconds(100),
                               [&] { return pending->done; })) {
        break;
      }
      if (std::chrono::steady_clock::now() >= deadline) {
        lock.unlock();
        {
          std::lock_guard state_lock(session->mutex);
          session->pending.erase(id);
        }
        throw std::runtime_error("the MCP server request timed out");
      }
    }
    if (pending->has_error) throw std::runtime_error(pending->error_message);
    return pending->result;
  }

  void notify(const std::shared_ptr<Session>& session, const std::string& method) {
    if (session->transport == "http") {
      http_send(session,
                {{"jsonrpc", "2.0"}, {"method", method}, {"params", nlohmann::json::object()}},
                std::nullopt, 20);
      return;
    }
    write_line(session, {{"jsonrpc", "2.0"}, {"method", method}});
  }

  void close_stdin(const std::shared_ptr<Session>& session) {
    std::lock_guard lock(session->io_mutex);
#ifdef _WIN32
    if (session->stdin_write != nullptr) {
      CloseHandle(session->stdin_write);
      session->stdin_write = nullptr;
    }
#else
    if (session->stdin_write >= 0) {
      close(session->stdin_write);
      session->stdin_write = -1;
    }
#endif
  }

  void shutdown(const std::shared_ptr<Session>& session) {
    if (!session) return;
    if (session->transport == "http") {
      const bool was_running = session->running;
      session->running = false;
      if (was_running) {
        try {
          delete_http_session(session);
        } catch (const std::exception&) {
          // A server that will not terminate the session cleanly is not an error
          // on shutdown; the process is going away either way.
        }
      }
      return;
    }
    close_stdin(session);
#ifdef _WIN32
    if (session->process != nullptr) {
      if (WaitForSingleObject(session->process, 2000) != WAIT_OBJECT_0) {
        TerminateProcess(session->process, 1);
        WaitForSingleObject(session->process, 2000);
      }
    }
#else
    if (session->pid > 0) {
      for (int attempt = 0; attempt < 40; ++attempt) {
        int status = 0;
        if (waitpid(session->pid, &status, WNOHANG) == session->pid) {
          session->pid = -1;
          break;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(50));
      }
      if (session->pid > 0) {
        kill(session->pid, SIGTERM);
        ::waitpid(session->pid, nullptr, 0);
        session->pid = -1;
      }
    }
#endif
    if (session->reader.joinable()) session->reader.join();
    if (session->stderr_reader.joinable()) session->stderr_reader.join();
#ifdef _WIN32
    if (session->stdout_read != nullptr) CloseHandle(session->stdout_read);
    if (session->stderr_read != nullptr) CloseHandle(session->stderr_read);
    if (session->process != nullptr) CloseHandle(session->process);
    session->stdout_read = nullptr;
    session->stderr_read = nullptr;
    session->process = nullptr;
#endif
  }

  void spawn_process(const std::shared_ptr<Session>& session, const std::string& command,
                     const nlohmann::json& args) {
#ifdef _WIN32
    SECURITY_ATTRIBUTES attributes{sizeof(SECURITY_ATTRIBUTES), nullptr, TRUE};
    HANDLE child_in = nullptr, child_out = nullptr, child_err = nullptr;
    HANDLE parent_in = nullptr, parent_out = nullptr, parent_err = nullptr;
    if (!CreatePipe(&child_in, &parent_in, &attributes, 0) ||
        !CreatePipe(&parent_out, &child_out, &attributes, 0) ||
        !CreatePipe(&parent_err, &child_err, &attributes, 0)) {
      throw std::runtime_error("could not create the MCP server pipes");
    }
    SetHandleInformation(parent_in, HANDLE_FLAG_INHERIT, 0);
    SetHandleInformation(parent_out, HANDLE_FLAG_INHERIT, 0);
    SetHandleInformation(parent_err, HANDLE_FLAG_INHERIT, 0);
    const auto wide = [](const std::string& value) {
      if (value.empty()) return std::wstring();
      const auto length = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                                              static_cast<int>(value.size()), nullptr, 0);
      if (length <= 0) throw std::runtime_error("the MCP command is not valid UTF-8");
      std::wstring result(static_cast<std::size_t>(length), L'\0');
      MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                          static_cast<int>(value.size()), result.data(), length);
      return result;
    };
    const auto quote = [](const std::wstring& value) {
      if (!value.empty() && value.find_first_of(L" \t") == std::wstring::npos) return value;
      std::wstring quoted = L"\"";
      std::size_t backslashes = 0;
      for (const auto character : value) {
        if (character == L'\\') {
          ++backslashes;
          continue;
        }
        if (character == L'"') {
          quoted.append(backslashes * 2 + 1, L'\\');
          quoted.push_back(L'"');
          backslashes = 0;
          continue;
        }
        quoted.append(backslashes, L'\\');
        backslashes = 0;
        quoted.push_back(character);
      }
      quoted.append(backslashes * 2, L'\\');
      quoted.push_back(L'"');
      return quoted;
    };
    const auto program = wide(command);
    std::wstring command_line = quote(program);
    for (const auto& argument : args) {
      command_line.push_back(L' ');
      command_line += quote(wide(argument.get<std::string>()));
    }
    STARTUPINFOW startup{};
    startup.cb = sizeof(startup);
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = child_in;
    startup.hStdOutput = child_out;
    startup.hStdError = child_err;
    PROCESS_INFORMATION info{};
    std::wstring mutable_command = command_line;
    if (!CreateProcessW(program.c_str(), mutable_command.data(), nullptr, nullptr, TRUE,
                        CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT, nullptr, nullptr, &startup,
                        &info)) {
      CloseHandle(child_in);
      CloseHandle(child_out);
      CloseHandle(child_err);
      CloseHandle(parent_in);
      CloseHandle(parent_out);
      CloseHandle(parent_err);
      throw std::runtime_error("could not start the MCP server process");
    }
    CloseHandle(child_in);
    CloseHandle(child_out);
    CloseHandle(child_err);
    CloseHandle(info.hThread);
    session->process = info.hProcess;
    session->process_id = static_cast<long long>(info.dwProcessId);
    session->program = command;
    session->started_at = iso8601_utc_now();
    session->stdin_write = parent_in;
    session->stdout_read = parent_out;
    session->stderr_read = parent_err;
#else
    int in_pipe[2] = {-1, -1}, out_pipe[2] = {-1, -1}, err_pipe[2] = {-1, -1};
    if (pipe(in_pipe) != 0 || pipe(out_pipe) != 0 || pipe(err_pipe) != 0) {
      throw std::runtime_error("could not create the MCP server pipes");
    }
    const auto child = fork();
    if (child < 0) throw std::runtime_error("could not fork the MCP server process");
    if (child == 0) {
      dup2(in_pipe[0], STDIN_FILENO);
      dup2(out_pipe[1], STDOUT_FILENO);
      dup2(err_pipe[1], STDERR_FILENO);
      close(in_pipe[0]);
      close(in_pipe[1]);
      close(out_pipe[0]);
      close(out_pipe[1]);
      close(err_pipe[0]);
      close(err_pipe[1]);
      std::vector<std::string> arguments;
      arguments.push_back(command);
      for (const auto& argument : args) arguments.push_back(argument.get<std::string>());
      std::vector<char*> argv;
      for (auto& argument : arguments) argv.push_back(argument.data());
      argv.push_back(nullptr);
      execvp(command.c_str(), argv.data());
      _exit(127);
    }
    close(in_pipe[0]);
    close(out_pipe[1]);
    close(err_pipe[1]);
    session->pid = child;
    session->process_id = static_cast<long long>(child);
    session->program = command;
    session->started_at = iso8601_utc_now();
    session->stdin_write = in_pipe[1];
    session->stdout_read = out_pipe[0];
    session->stderr_read = err_pipe[0];
#endif
  }

  // Performs the initialize handshake and the paginated tools/list walk for a
  // session, whatever its transport (request/notify dispatch on `transport`).
  void handshake_and_list(const std::shared_ptr<Session>& session) {
    const auto initialized = request(
        session, "initialize",
        {{"protocolVersion", kProtocolVersion},
         {"capabilities", nlohmann::json::object()},
         {"clientInfo", {{"name", kClientName}, {"version", kClientVersion}}}},
        20);
    session->protocol_version = initialized.value("protocolVersion", std::string());
    if (initialized.contains("serverInfo") && initialized.at("serverInfo").is_object()) {
      session->server_name = initialized.at("serverInfo").value("name", std::string());
      session->server_version = initialized.at("serverInfo").value("version", std::string());
    }
    notify(session, "notifications/initialized");
    session->tools = nlohmann::json::array();
    std::string cursor;
    for (int page = 0; page < 100; ++page) {
      nlohmann::json list_params = nlohmann::json::object();
      if (!cursor.empty()) list_params["cursor"] = cursor;
      const auto listed = request(session, "tools/list", list_params, 20);
      if (listed.is_object() && listed.contains("tools") && listed.at("tools").is_array()) {
        for (const auto& tool : listed.at("tools")) session->tools.push_back(tool);
        if (session->tools.size() > 10000) {
          throw std::runtime_error("the MCP server exposed too many tools");
        }
      }
      std::string next;
      if (listed.is_object() && listed.contains("nextCursor") &&
          listed.at("nextCursor").is_string()) {
        next = listed.at("nextCursor").get<std::string>();
      }
      if (next.empty()) break;
      cursor = next;
    }
  }

  std::shared_ptr<Session> start_http_server(const nlohmann::json& row) {
    const auto server_id = row.value("id", std::string());
    const auto url = row.value("url", std::string());
    HttpUrlParts parts;
    if (!parse_http_url(url, parts)) {
      throw std::runtime_error("the MCP HTTP url is invalid");
    }
    auto session = std::make_shared<Session>();
    session->server_id = server_id;
    session->segment = sanitize_segment(server_id);
    session->label = row.value("label", server_id);
    session->annotation_trust = row.value("annotationTrust", std::string("none"));
    session->allowed_tools = row.value("allowedTools", nlohmann::json::array());
    session->transport = "http";
    session->http_origin = parts.origin;
    session->http_path = parts.path;
    session->running = true;
    session->started_at = iso8601_utc_now();
    // Static auth headers: the reference is resolved against the OS credential
    // store, so the plaintext never leaves it. A missing secret is not fatal;
    // the server's own 401 is the real answer. A stored value is re-validated
    // here as well as at save time, because a row can predate that check or be
    // written by another host version: `httplib::write_headers` copies both
    // name and value verbatim, so a CR/LF/NUL there would inject headers. One
    // bad header is dropped (closed), never the whole server.
    for (const auto& header : row.value("httpAuthHeaders", nlohmann::json::array())) {
      if (!header.is_object()) continue;
      const auto name = header.value("name", std::string());
      const auto reference = header.value("credentialRef", std::string());
      if (name.empty() || reference.empty()) continue;
      if (name.size() > 128 || has_header_control_characters(name)) {
        session->diagnostics.push_back(
            "dropped a static header whose name contains control characters");
        continue;
      }
      try {
        const auto secret = credentials->get(CredentialReference::parse(reference));
        std::string value(reinterpret_cast<const char*>(secret.data()), secret.size());
        if (value.size() > 8192 || has_header_control_characters(value)) {
          session->diagnostics.push_back(
              "dropped the static header \"" + name +
              "\" because its stored secret contains control characters");
          continue;
        }
        session->http_headers.emplace_back(name, std::move(value));
      } catch (const std::exception&) {
        continue;
      }
    }
    // A connected OAuth configuration adds a dynamic token source: the access
    // token is refreshed on demand and, when the server asked for
    // sender-constrained tokens, a per-request DPoP proof is signed. Building it
    // fails loudly so a broken token endpoint surfaces at start, not on the
    // first tool call.
    if (row.contains("oauth") && row.at("oauth").is_object() &&
        !row.at("oauth").value("credentialRef", std::string()).empty()) {
      session->oauth = std::make_unique<OAuthTokenSource>(credentials, row.at("oauth"), url);
    }
    try {
      handshake_and_list(session);
    } catch (...) {
      session->running = false;
      throw;
    }
    {
      std::lock_guard lock(mutex);
      sessions[server_id] = session;
    }
    return session;
  }

  std::shared_ptr<Session> start_server(const nlohmann::json& row) {
    const auto server_id = row.value("id", std::string());
    const auto transport = row.value("transport", std::string());
    if (transport == "http") return start_http_server(row);
    if (transport != "stdio") {
      throw std::runtime_error("the MCP server has an unsupported transport");
    }
    const auto command = row.value("command", std::string());
    if (command.empty()) throw std::runtime_error("the MCP server has no command");
    auto session = std::make_shared<Session>();
    session->server_id = server_id;
    session->segment = sanitize_segment(server_id);
    session->label = row.value("label", server_id);
    session->annotation_trust = row.value("annotationTrust", std::string("none"));
    session->allowed_tools = row.value("allowedTools", nlohmann::json::array());
    session->running = true;
    const auto args = row.value("args", nlohmann::json::array());
    spawn_process(session, command, args);
    session->reader = std::thread([this, session] { reader_loop(session); });
    session->stderr_reader = std::thread([this, session] { stderr_loop(session); });
    try {
      handshake_and_list(session);
    } catch (...) {
      shutdown(session);
      throw;
    }
    {
      std::lock_guard lock(mutex);
      sessions[server_id] = session;
    }
    return session;
  }

  std::shared_ptr<Session> ensure_started(const nlohmann::json& row) {
    const auto server_id = row.value("id", std::string());
    {
      std::lock_guard lock(mutex);
      const auto found = sessions.find(server_id);
      if (found != sessions.end() && found->second->running) return found->second;
      if (found != sessions.end()) sessions.erase(found);
    }
    return start_server(row);
  }

  void stop_server(const std::string& server_id) {
    std::shared_ptr<Session> session;
    {
      std::lock_guard lock(mutex);
      const auto found = sessions.find(server_id);
      if (found == sessions.end()) return;
      session = found->second;
      sessions.erase(found);
    }
    shutdown(session);
  }

  nlohmann::json status_for(const std::shared_ptr<Session>& session) {
    std::lock_guard lock(session->mutex);
    return {{"serverId", session->server_id},
            {"running", session->running},
            // Only a live stdio session has a local process to report; an HTTP
            // session and a dead one answer null, never a fabricated id.
            {"pid", session->running && session->process_id > 0
                        ? nlohmann::json(session->process_id)
                        : nlohmann::json(nullptr)},
            {"program", session->running && !session->program.empty()
                            ? nlohmann::json(session->program)
                            : nlohmann::json(nullptr)},
            {"startedAt", session->running && !session->started_at.empty()
                              ? nlohmann::json(session->started_at)
                              : nlohmann::json(nullptr)},
            {"protocolVersion", session->protocol_version.empty()
                                    ? nlohmann::json(nullptr)
                                    : nlohmann::json(session->protocol_version)},
            {"serverName", session->server_name.empty() ? nlohmann::json(nullptr)
                                                        : nlohmann::json(session->server_name)},
            {"serverVersion", session->server_version.empty()
                                  ? nlohmann::json(nullptr)
                                  : nlohmann::json(session->server_version)},
            {"toolCount", session->tools.size()},
            {"stderrTail", session->stderr_tail},
            {"diagnostics", session->diagnostics}};
  }
};

McpSupervisor::McpSupervisor(DesktopDatabase* database, const CredentialStore* credentials)
    : impl_(std::make_unique<Impl>()) {
  impl_->database = database;
  impl_->credentials = credentials;
}

McpSupervisor::~McpSupervisor() = default;

nlohmann::json McpSupervisor::start(const std::string& server_id) {
  const auto row = impl_->database->get_mcp_server(server_id);
  if (!row.has_value()) throw std::runtime_error("the MCP server was not found");
  const auto session = impl_->ensure_started(*row);
  return {{"config", *row}, {"status", impl_->status_for(session)}, {"tools", nlohmann::json::array()}};
}

nlohmann::json McpSupervisor::stop(const std::string& server_id) {
  bool was_running = false;
  {
    std::lock_guard lock(impl_->mutex);
    was_running = impl_->sessions.find(server_id) != impl_->sessions.end();
  }
  impl_->stop_server(server_id);
  const auto row = impl_->database->get_mcp_server(server_id);
  if (!row.has_value()) throw std::runtime_error("the MCP server was not found");
  nlohmann::json status = {{"serverId", server_id},
                           {"running", false},
                           {"toolCount", 0},
                           {"stderrTail", nlohmann::json::array()},
                           {"diagnostics", nlohmann::json::array()}};
  return {{"server",
           {{"config", *row}, {"status", std::move(status)}, {"tools", nlohmann::json::array()}}},
          {"shutdown",
           {{"wasRunning", was_running}, {"killed", false}, {"unreaped", false}}}};
}

std::optional<nlohmann::json> McpSupervisor::status(const std::string& server_id) {
  std::shared_ptr<Impl::Session> session;
  {
    std::lock_guard lock(impl_->mutex);
    const auto found = impl_->sessions.find(server_id);
    if (found == impl_->sessions.end()) return std::nullopt;
    session = found->second;
  }
  // A session whose reader observed the child exit is genuinely stopped, even
  // though it is still mapped. Report it as absent so the caller answers with the
  // stopped shape rather than a running one.
  {
    std::lock_guard lock(session->mutex);
    if (!session->running) return std::nullopt;
  }
  return impl_->status_for(session);
}

nlohmann::json McpSupervisor::catalog() {
  nlohmann::json servers = nlohmann::json::array();
  nlohmann::json failures = nlohmann::json::array();
  const auto rows = impl_->database->list_mcp_servers().at("servers");
  for (const auto& row : rows) {
    if (!row.value("enabled", false)) continue;
    const auto allowed = row.value("allowedTools", nlohmann::json::array());
    if (!allowed.is_array() || allowed.empty()) continue;
    std::shared_ptr<Impl::Session> session;
    try {
      session = impl_->ensure_started(row);
    } catch (const std::exception& error) {
      failures.push_back({{"serverId", row.value("id", std::string())},
                          {"code", "launch_failed"},
                          {"message", error.what()}});
      continue;
    }
    nlohmann::json tools = nlohmann::json::array();
    for (const auto& descriptor : session->tools) {
      if (!descriptor.is_object()) continue;
      const auto remote = descriptor.value("name", std::string());
      if (remote.empty()) continue;
      const bool reviewed = std::any_of(allowed.begin(), allowed.end(), [&](const auto& item) {
        return item.is_string() && item.template get<std::string>() == remote;
      });
      if (!reviewed) continue;
      const auto annotations =
          descriptor.contains("annotations") ? descriptor.at("annotations") : nlohmann::json::object();
      tools.push_back({{"name", internal_tool_name(session->segment, remote)},
                       {"serverId", row.value("id", std::string())},
                       {"tool", remote},
                       {"description", descriptor.value("description", std::string())},
                       {"inputSchema", descriptor.contains("inputSchema")
                                           ? descriptor.at("inputSchema")
                                           : nlohmann::json::object()},
                       {"risk", effective_risk(session->annotation_trust, annotations)}});
    }
    servers.push_back({{"serverId", row.value("id", std::string())},
                       {"segment", session->segment},
                       {"label", row.value("label", std::string())},
                       {"tools", std::move(tools)}});
  }
  return {{"servers", std::move(servers)}, {"failures", std::move(failures)}};
}

std::optional<std::string> McpSupervisor::tool_effective_risk(const std::string& internal_name) {
  std::string server_segment;
  std::string tool_segment;
  if (!split_internal_name(internal_name, server_segment, tool_segment)) return std::nullopt;
  std::shared_ptr<Impl::Session> session;
  {
    std::lock_guard lock(impl_->mutex);
    for (const auto& [id, candidate] : impl_->sessions) {
      if (candidate->segment == server_segment && candidate->running) {
        session = candidate;
        break;
      }
    }
  }
  if (!session) return std::nullopt;
  for (const auto& descriptor : session->tools) {
    if (!descriptor.is_object() ||
        sanitize_segment(descriptor.value("name", std::string())) != tool_segment) {
      continue;
    }
    const auto remote = descriptor.value("name", std::string());
    const bool reviewed = std::any_of(session->allowed_tools.begin(), session->allowed_tools.end(),
                                      [&](const auto& item) {
                                        return item.is_string() &&
                                               item.template get<std::string>() == remote;
                                      });
    if (!reviewed) return std::nullopt;
    const auto annotations = descriptor.contains("annotations") ? descriptor.at("annotations")
                                                                : nlohmann::json::object();
    return effective_risk(session->annotation_trust, annotations);
  }
  return std::nullopt;
}

nlohmann::json McpSupervisor::call(const std::string& internal_name,
                                   const nlohmann::json& arguments,
                                   const std::shared_ptr<std::atomic<bool>>& cancel) {
  std::string server_segment;
  std::string tool_segment;
  if (!split_internal_name(internal_name, server_segment, tool_segment)) {
    throw std::runtime_error("the tool name is not an MCP tool");
  }
  std::shared_ptr<Impl::Session> session;
  {
    std::lock_guard lock(impl_->mutex);
    for (const auto& [id, candidate] : impl_->sessions) {
      if (candidate->segment == server_segment && candidate->running) {
        session = candidate;
        break;
      }
    }
  }
  if (!session) throw std::runtime_error("the MCP server is not running");
  std::string remote;
  bool reviewed = false;
  for (const auto& descriptor : session->tools) {
    if (descriptor.is_object() &&
        sanitize_segment(descriptor.value("name", std::string())) == tool_segment) {
      remote = descriptor.value("name", std::string());
      reviewed = std::any_of(session->allowed_tools.begin(), session->allowed_tools.end(),
                             [&](const auto& item) {
                               return item.is_string() && item.template get<std::string>() == remote;
                             });
      break;
    }
  }
  if (remote.empty()) throw std::runtime_error("the MCP server does not expose that tool");
  if (!reviewed) throw std::runtime_error("that MCP tool has not been reviewed");
  const auto result = impl_->request(
      session, "tools/call", {{"name", remote}, {"arguments", arguments}}, 60, cancel);
  const bool is_error = result.value("isError", false);
  nlohmann::json content = nlohmann::json::array();
  std::string text;
  if (result.contains("content") && result.at("content").is_array()) {
    for (const auto& block : result.at("content")) {
      if (!block.is_object()) continue;
      if (block.value("type", std::string()) == "text" && block.contains("text") &&
          block.at("text").is_string()) {
        const auto value = block.at("text").get<std::string>();
        if (!text.empty()) text.push_back('\n');
        text += value;
        content.push_back({{"type", "text"}, {"text", value}});
      } else {
        content.push_back({{"type", "other"}, {"value", block}});
      }
    }
  }
  nlohmann::json output = {{"serverId", session->server_id},
                           {"tool", remote},
                           {"isError", is_error},
                           {"text", text},
                           {"content", std::move(content)}};
  if (result.contains("structuredContent")) {
    output["structuredContent"] = result.at("structuredContent");
  }
  return output;
}

}  // namespace yukinal::desktop_host

#include "yukinal/desktop_host/desktop_mcp.hpp"

#include "yukinal/desktop_host/credential_store.hpp"
#include "yukinal/desktop_host/database.hpp"

#include <httplib.h>

#include <algorithm>
#include <atomic>
#include <cctype>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <cstdlib>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <random>
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
constexpr const char* kClientVersion = "1.0.0";

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
    auto response =
        client.Post(session->http_path, http_headers_for(session), body, "application/json");
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

  nlohmann::json http_request(const std::shared_ptr<Session>& session, const std::string& method,
                              const nlohmann::json& params, int timeout_seconds) {
    long long id = 0;
    {
      std::lock_guard lock(session->mutex);
      if (!session->running) throw std::runtime_error("the MCP server is not running");
      id = session->next_id++;
    }
    return http_send(session,
                     {{"jsonrpc", "2.0"}, {"id", id}, {"method", method}, {"params", params}},
                     id, timeout_seconds);
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
    auto response = client.Delete(session->http_path, http_headers_for(session));
    if (!response) return;
    const auto status = response->status;
    if ((status >= 200 && status < 300) || status == 405 || status == 404) {
      std::lock_guard lock(session->mutex);
      session->http_session_id.clear();
    }
  }

  nlohmann::json request(const std::shared_ptr<Session>& session, const std::string& method,
                         const nlohmann::json& params, int timeout_seconds) {
    if (session->transport == "http") {
      return http_request(session, method, params, timeout_seconds);
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
    if (!pending->cv.wait_for(lock, std::chrono::seconds(timeout_seconds),
                              [&] { return pending->done; })) {
      std::lock_guard state_lock(session->mutex);
      session->pending.erase(id);
      throw std::runtime_error("the MCP server request timed out");
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
    // Static auth headers: the reference is resolved against the OS credential
    // store, so the plaintext never leaves it. A missing secret is not fatal;
    // the server's own 401 is the real answer.
    for (const auto& header : row.value("httpAuthHeaders", nlohmann::json::array())) {
      if (!header.is_object()) continue;
      const auto name = header.value("name", std::string());
      const auto reference = header.value("credentialRef", std::string());
      if (name.empty() || reference.empty()) continue;
      try {
        const auto secret = credentials->get(CredentialReference::parse(reference));
        std::string value(reinterpret_cast<const char*>(secret.data()), secret.size());
        session->http_headers.emplace_back(name, std::move(value));
      } catch (const std::exception&) {
        continue;
      }
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
            {"pid", nullptr},
            {"program", nullptr},
            {"startedAt", nullptr},
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
        return item.is_string() && item.get<std::string>() == remote;
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

nlohmann::json McpSupervisor::call(const std::string& internal_name,
                                   const nlohmann::json& arguments) {
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
                               return item.is_string() && item.get<std::string>() == remote;
                             });
      break;
    }
  }
  if (remote.empty()) throw std::runtime_error("the MCP server does not expose that tool");
  if (!reviewed) throw std::runtime_error("that MCP tool has not been reviewed");
  const auto result = impl_->request(
      session, "tools/call", {{"name", remote}, {"arguments", arguments}}, 60);
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

#include "yukinal/desktop_host/desktop_terminal.hpp"

#include "terminal_exit.hpp"

#include "yukinal/desktop_host/credential_store.hpp"
#include "yukinal/desktop_host/database.hpp"
#include "yukinal/desktop_host/desktop_target.hpp"

#include <algorithm>
#include <atomic>
#include <cctype>
#include <chrono>
#include <cstdint>
#include <cstdlib>
#include <ctime>
#include <filesystem>
#include <fstream>
#include <limits>
#include <map>
#include <mutex>
#include <random>
#include <stdexcept>
#include <thread>
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

constexpr std::size_t kMaxFrameBytes = 1024 * 1024;
constexpr std::size_t kMaxLogLines = 40;
constexpr std::size_t kMaxLogLineBytes = 4000;
// A terminal session owns a child process, two pipe handles and a reader thread.
// Bound the number that may exist at once so a UI loop cannot exhaust the host;
// the count includes sessions whose reader has ended but whose process has not
// been reaped yet, because those still hold those resources.
constexpr std::size_t kMaxTerminalSessions = 8;

// The desktop event schema takes `expiresAt` as an ISO-8601 UTC instant; it is
// display metadata, so a fixed second-resolution rendering is enough.
std::string iso8601_utc(std::chrono::system_clock::time_point when) {
  const auto seconds = std::chrono::system_clock::to_time_t(when);
  std::tm parts{};
#ifdef _WIN32
  gmtime_s(&parts, &seconds);
#else
  gmtime_r(&seconds, &parts);
#endif
  char buffer[32] = {};
  if (std::strftime(buffer, sizeof(buffer), "%Y-%m-%dT%H:%M:%SZ", &parts) == 0) return {};
  return buffer;
}

int base64_value(char character) {
  if (character >= 'A' && character <= 'Z') return character - 'A';
  if (character >= 'a' && character <= 'z') return character - 'a' + 26;
  if (character >= '0' && character <= '9') return character - '0' + 52;
  if (character == '+') return 62;
  if (character == '/') return 63;
  return -1;
}

std::vector<std::uint8_t> base64_decode(const std::string& text) {
  std::vector<std::uint8_t> bytes;
  int buffer = 0;
  int bits = 0;
  for (const char character : text) {
    if (character == '=') break;
    if (std::isspace(static_cast<unsigned char>(character)) != 0) continue;
    const int value = base64_value(character);
    if (value < 0) return {};
    buffer = (buffer << 6) | value;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      bytes.push_back(static_cast<std::uint8_t>((buffer >> bits) & 0xFF));
    }
  }
  return bytes;
}

std::string utf8_lossy(const std::vector<std::uint8_t>& bytes) {
  static constexpr char kReplacement[] = "\xEF\xBF\xBD";
  std::string out;
  out.reserve(bytes.size());
  std::size_t index = 0;
  while (index < bytes.size()) {
    const auto byte = bytes[index];
    if (byte < 0x80) {
      out.push_back(static_cast<char>(byte));
      ++index;
      continue;
    }
    int length = 0;
    std::uint32_t code_point = 0;
    std::uint32_t minimum = 0;
    if ((byte & 0xE0) == 0xC0) {
      length = 2;
      code_point = byte & 0x1Fu;
      minimum = 0x80;
    } else if ((byte & 0xF0) == 0xE0) {
      length = 3;
      code_point = byte & 0x0Fu;
      minimum = 0x800;
    } else if ((byte & 0xF8) == 0xF0) {
      length = 4;
      code_point = byte & 0x07u;
      minimum = 0x10000;
    } else {
      out += kReplacement;
      ++index;
      continue;
    }
    if (index + static_cast<std::size_t>(length) > bytes.size()) {
      out += kReplacement;
      break;
    }
    bool valid = true;
    for (int offset = 1; offset < length; ++offset) {
      const auto continuation = bytes[index + static_cast<std::size_t>(offset)];
      if ((continuation & 0xC0) != 0x80) {
        valid = false;
        break;
      }
      code_point = (code_point << 6) | (continuation & 0x3Fu);
    }
    if (!valid || code_point < minimum || code_point > 0x10FFFF ||
        (code_point >= 0xD800 && code_point <= 0xDFFF)) {
      out += kReplacement;
      ++index;
      continue;
    }
    out.append(reinterpret_cast<const char*>(bytes.data() + index),
               static_cast<std::size_t>(length));
    index += static_cast<std::size_t>(length);
  }
  return out;
}

std::string new_session_id() {
  std::random_device device;
  static constexpr char kHex[] = "0123456789abcdef";
  std::string id = "term_";
  id.reserve(5 + 32);
  for (int index = 0; index < 16; ++index) {
    const auto byte = static_cast<unsigned>(device() & 0xFFu);
    id.push_back(kHex[byte >> 4]);
    id.push_back(kHex[byte & 0x0Fu]);
  }
  return id;
}

#ifdef _WIN32
std::wstring to_wide(const std::string& value) {
  if (value.empty()) return {};
  const auto length = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                                          static_cast<int>(value.size()), nullptr, 0);
  if (length <= 0) throw std::runtime_error("the terminal argument is not valid UTF-8");
  std::wstring result(static_cast<std::size_t>(length), L'\0');
  MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                      static_cast<int>(value.size()), result.data(), length);
  return result;
}

std::wstring quote_argument(const std::wstring& value) {
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
}
#else
std::string quote_argument(const std::string& value) { return value; }
#endif

}  // namespace

struct TerminalManager::Impl {
  struct Session {
    std::string server_id;
    // The target metadata the service was started with; the challenge event is
    // the only place the UI sees them, so they are kept here at spawn time.
    std::string username;
    std::string host;
    int cols = 0;
    int rows = 0;
    std::filesystem::path config_path;
    std::mutex mutex;
    std::atomic<bool> closed{false};
    // The service's own `closed` frame is the only authoritative source for the
    // remote shell's exit status; it distinguishes a frame that carried null
    // from no frame at all. The wrapper process exits with its own status and
    // must never be reported as the remote result. Guarded by `mutex`.
    terminal_detail::TerminalExitTracker remote_exit;
    // Cleared exactly once, by the reader after it reaps the process or by
    // `stop_session`, whichever happens first. Guarantees the live-session
    // counter is released once even when a session ends on its own and is never
    // explicitly closed before the manager is destroyed.
    std::atomic<bool> slot_held{true};
    std::vector<std::string> diagnostics;
#ifdef _WIN32
    HANDLE process = nullptr;
    HANDLE stdin_write = nullptr;
    HANDLE stdout_read = nullptr;
    std::thread reader;
#else
    pid_t pid = -1;
    int stdin_write = -1;
    int stdout_read = -1;
    std::thread reader;
#endif
  };

  DesktopDatabase* database = nullptr;
  const CredentialStore* credentials = nullptr;
  EventSink sink;
  std::mutex mutex;
  std::map<std::string, std::shared_ptr<Session>> sessions;
  // Guarded by `mutex`. Counts every session that owns a child process or a
  // reader thread, including ones already erased from `sessions` by `close()`
  // while `stop_session` is still reaping them.
  std::size_t live_sessions = 0;
  // Guarded by `mutex`, like `sessions`.
  AuthChallenges challenges;

  ~Impl() {
    std::map<std::string, std::shared_ptr<Session>> snapshot;
    {
      std::lock_guard lock(mutex);
      snapshot.swap(sessions);
    }
    for (auto& [id, session] : snapshot) stop_session(session);
  }

  void emit(const std::string& event, const nlohmann::json& payload) {
    EventSink current;
    {
      std::lock_guard lock(mutex);
      current = sink;
    }
    if (!current) return;
    try {
      current(event, payload);
    } catch (...) {
      // A UI delivery failure must not tear down the terminal transport.
    }
  }

  // Releases the session's slot exactly once. The reader calls this as soon as
  // the wrapper process has been reaped; `stop_session` calls it as well, and
  // the atomic makes the second call a no-op. Counting until the reap (not
  // until `close`) is what keeps the bound real under open/close churn.
  void release_session_slot(const std::shared_ptr<Session>& session) noexcept {
    if (!session || !session->slot_held.exchange(false)) return;
    std::lock_guard lock(mutex);
    if (live_sessions > 0) --live_sessions;
  }

  // For a slot claimed by `open_session` before a `Session` exists.
  void release_unclaimed_slot() noexcept {
    std::lock_guard lock(mutex);
    if (live_sessions > 0) --live_sessions;
  }

  void record_diagnostic(const std::shared_ptr<Session>& session, const std::string& line) {
    std::lock_guard lock(session->mutex);
    session->diagnostics.push_back(line.size() > kMaxLogLineBytes ? line.substr(0, kMaxLogLineBytes)
                                                                  : line);
    while (session->diagnostics.size() > kMaxLogLines) session->diagnostics.erase(session->diagnostics.begin());
  }

  bool write_frame(const std::shared_ptr<Session>& session, const nlohmann::json& frame) {
    const auto payload = frame.dump() + "\n";
    std::lock_guard lock(session->mutex);
    if (session->closed.load()) return false;
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

  void close_stdin(const std::shared_ptr<Session>& session) {
    std::lock_guard lock(session->mutex);
#ifdef _WIN32
    if (session->stdin_write != nullptr) {
      CloseHandle(session->stdin_write);
      session->stdin_write = nullptr;
    }
#else
    if (session->stdin_write >= 0) {
      ::close(session->stdin_write);
      session->stdin_write = -1;
    }
#endif
  }

  void stop_session(const std::shared_ptr<Session>& session) {
    if (!session) return;
    write_frame(session, {{"type", "close"}});
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
    std::error_code error;
    if (!session->config_path.empty()) std::filesystem::remove(session->config_path, error);
#ifdef _WIN32
    if (session->stdout_read != nullptr) CloseHandle(session->stdout_read);
    if (session->process != nullptr) CloseHandle(session->process);
    session->stdout_read = nullptr;
    session->process = nullptr;
#endif
    // The process has been reaped and the reader joined, so this session no
    // longer occupies a slot in the live-session bound.
    release_session_slot(session);
  }

  void reader_loop(const std::shared_ptr<Session>& session) {
    std::string buffer;
    std::string chunk;
    while (read_stdout(session, chunk)) {
      buffer += chunk;
      chunk.clear();
      std::size_t start = 0;
      for (;;) {
        const auto newline = buffer.find('\n', start);
        if (newline == std::string::npos) break;
        auto line = buffer.substr(start, newline - start);
        if (!line.empty() && line.back() == '\r') line.pop_back();
        handle_frame(session, line);
        start = newline + 1;
      }
      if (start > 0) buffer.erase(0, start);
      if (buffer.size() > kMaxFrameBytes) buffer.clear();
    }
    if (!buffer.empty()) handle_frame(session, buffer);

    // Reap the wrapper so it does not linger, but do not read its exit status:
    // that status belongs to `yukinal-ssh-service`, not to the remote shell.
    // The service writes its `closed` frame before exiting, so only that frame
    // (stored by `handle_frame`) may fill `exitCode`.
    bool reaped = false;
#ifdef _WIN32
    if (session->process != nullptr) {
      reaped = WaitForSingleObject(session->process, 5000) == WAIT_OBJECT_0;
    }
#else
    if (session->pid > 0) {
      int status = 0;
      if (waitpid(session->pid, &status, 0) == session->pid) {
        session->pid = -1;
        reaped = true;
      }
    }
#endif
    // Only a reaped process frees its slot. If the wrapper is still alive here,
    // `stop_session` will reap it and release the slot; otherwise an exhausted
    // cap could outlive the processes it was protecting. The release is
    // idempotent, so `stop_session` may also call it.
    if (reaped) release_session_slot(session);
    // Resolve the id before taking `session->mutex`: `session_id_of` locks the
    // manager mutex, and no path may take the two in both orders.
    const auto session_id = session_id_of(session);
    nlohmann::json exit_payload;
    {
      std::lock_guard lock(session->mutex);
      // Absence of the frame means "unknown", which the shared schema
      // represents as null. Guessing zero here is what made a failed remote
      // shell look clean.
      exit_payload = session->remote_exit.payload(session_id);
    }
    session->closed.store(true);
    emit("terminal.closed", std::move(exit_payload));
  }

  std::string session_id_of(const std::shared_ptr<Session>& session) {
    std::lock_guard lock(mutex);
    for (const auto& [id, candidate] : sessions) {
      if (candidate == session) return id;
    }
    return {};
  }

  void handle_frame(const std::shared_ptr<Session>& session, const std::string& line) {
    if (line.empty() || line.size() > kMaxFrameBytes) return;
    nlohmann::json frame;
    try {
      frame = nlohmann::json::parse(line);
    } catch (const nlohmann::json::exception&) {
      record_diagnostic(session, "dropped malformed terminal frame");
      return;
    }
    if (!frame.is_object() || !frame.contains("type") || !frame.at("type").is_string()) return;
    const auto type = frame.at("type").get<std::string>();
    const auto session_id = session_id_of(session);
    if (session_id.empty()) return;
    if (type == "ready") {
      emit("terminal.opened", {{"terminalSessionId", session_id},
                               {"serverId", session->server_id},
                               {"cols", session->cols},
                               {"rows", session->rows}});
      return;
    }
    if (type == "output" && frame.contains("data") && frame.at("data").is_string()) {
      const auto bytes = base64_decode(frame.at("data").get<std::string>());
      emit("terminal.data", {{"terminalSessionId", session_id}, {"data", utf8_lossy(bytes)}});
      return;
    }
    if (type == "closed") {
      // The service writes this frame before it exits. Its `exit_code` is the
      // remote shell's status, or null when the peer closed without sending
      // one. The wrapper's own process exit status is never substituted.
      std::string warning;
      bool accepted = false;
      {
        std::lock_guard lock(session->mutex);
        accepted = session->remote_exit.observe_frame(frame, warning);
      }
      // Diagnostics are recorded outside `session->mutex`: the helper takes it.
      if (!accepted) {
        record_diagnostic(session, "dropped a terminal closed frame without a valid exit code");
      } else if (!warning.empty()) {
        record_diagnostic(session, warning);
      }
      // The worker's terminal loop has ended. Close our write end so its stdin
      // reader can observe EOF and the child can finish exiting; otherwise the
      // reader thread can keep the worker alive after the remote shell closes.
      session->closed.store(true);
      close_stdin(session);
      return;
    }
    if (type == "error") {
      record_diagnostic(session, frame.value("message", std::string("terminal error")));
      return;
    }
    if (type == "auth_prompt") {
      // Keyboard-interactive: the worker is waiting for credentials. Register the
      // challenge and hand it to the UI; the answer comes back through
      // `server_auth_respond` as a separate IPC command.
      auto prompts = nlohmann::json::array();
      std::size_t expected = 0;
      if (frame.contains("prompts") && frame.at("prompts").is_array()) {
        for (const auto& prompt : frame.at("prompts")) {
          if (!prompt.is_object()) continue;
          prompts.push_back({{"prompt", prompt.value("prompt", std::string())},
                             {"echo", prompt.value("echo", false)}});
          ++expected;
        }
      }
      if (expected == 0) {
        // Nothing to ask means nothing the UI could answer; say so instead of
        // registering a challenge that can never be completed.
        record_diagnostic(session, "dropped an authentication prompt without prompts");
        return;
      }
      std::string auth_id;
      {
        std::lock_guard lock(mutex);
        auth_id = challenges.begin(session_id, expected, std::chrono::steady_clock::now());
      }
      const auto name = frame.value("name", std::string());
      const auto instructions = frame.value("instructions", std::string());
      emit("server.auth_challenge",
           {{"authId", auth_id},
            {"serverId", session->server_id},
            // The schema wants non-empty display strings. Fall back to the
            // server's own metadata, then to the worker's prompt name, then to a
            // literal — never to an empty string the UI would render as blank.
            {"username", !session->username.empty() ? session->username
                                                     : (!name.empty() ? name : std::string("unknown"))},
            {"host", !session->host.empty() ? session->host : session->server_id},
            {"name", name},
            {"instructions", instructions},
            {"prompts", std::move(prompts)},
            {"expiresAt",
             iso8601_utc(std::chrono::system_clock::now() + AuthChallenges::kLifetime)}});
      return;
    }
  }

  bool read_stdout(const std::shared_ptr<Session>& session, std::string& chunk) {
    chunk.clear();
#ifdef _WIN32
    HANDLE handle = nullptr;
    {
      std::lock_guard lock(session->mutex);
      handle = session->stdout_read;
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
      std::lock_guard lock(session->mutex);
      descriptor = session->stdout_read;
    }
    if (descriptor < 0) return false;
    char buffer[16 * 1024];
    const auto read = ::read(descriptor, buffer, sizeof(buffer));
    if (read <= 0) return false;
    chunk.assign(buffer, buffer + read);
    return true;
#endif
  }

  std::filesystem::path resolve_binary() const {
#ifdef _WIN32
    if (const auto* env = std::getenv("YUKINAL_SSH_SERVICE_BINARY");
        env != nullptr && *env != '\0') {
      return std::filesystem::path(env);
    }
    wchar_t module[MAX_PATH]{};
    if (GetModuleFileNameW(nullptr, module, MAX_PATH) > 0) {
      auto candidate = std::filesystem::path(module).parent_path() / "yukinal-ssh-service.exe";
      if (std::filesystem::exists(candidate)) return candidate;
    }
    return {};
#else
    if (const auto* env = std::getenv("YUKINAL_SSH_SERVICE_BINARY");
        env != nullptr && *env != '\0') {
      return std::filesystem::path(env);
    }
    return {};
#endif
  }

  // Releases a live-session slot on every failure path. It starts out covering
  // the bare claim and is re-armed with the `Session` once one exists, so a
  // throw anywhere between spawn and thread start cannot leak a slot. Both
  // releases are idempotent, so it may coexist with `stop_session`.
  struct SessionSlot {
    Impl* owner = nullptr;
    std::shared_ptr<Session> session;
    bool active = true;
    ~SessionSlot() {
      if (!active) return;
      if (session) {
        owner->release_session_slot(session);
      } else {
        owner->release_unclaimed_slot();
      }
    }
    void arm(std::shared_ptr<Session> spawned) noexcept {
      session = std::move(spawned);
    }
    void dismiss() noexcept {
      active = false;
      session.reset();
    }
  };

  std::string open_session(const std::string& server_id, int cols, int rows) {
    if (cols < 1 || cols > 1000 || rows < 1 || rows > 1000) {
      throw std::runtime_error("the terminal dimensions are invalid");
    }
    // The bound is claimed before the worker is spawned so two concurrent opens
    // cannot both pass the check. Sessions that were closed but not yet reaped
    // still hold a process and a thread, so they keep their slot until the
    // process is reaped by `stop_session` or the reader.
    {
      std::lock_guard lock(mutex);
      if (live_sessions >= kMaxTerminalSessions) {
        throw std::runtime_error(
            "the desktop host already has the maximum number of terminal sessions");
      }
      ++live_sessions;
    }
    // Armed only after the claim: a refused open must not release a slot it
    // never took.
    SessionSlot slot{this};
    const auto binary = resolve_binary();
    if (binary.empty() || !std::filesystem::is_regular_file(binary)) {
      throw std::runtime_error("the packaged C++ SSH service executable is unavailable");
    }
    const auto payload = resolve_ssh_service_target(*database, *credentials, server_id);
    const auto session_id = new_session_id();
    const auto config_path =
        std::filesystem::temp_directory_path() / ("yukinal-ssh-terminal-" + session_id + ".json");
    {
      std::ofstream config(config_path, std::ios::binary | std::ios::trunc);
      config << nlohmann::json{{"listen_port", 8765},
                               {"api_token_env", "YUKINAL_SSH_API_TOKEN"},
                               {"targets", nlohmann::json::array({payload.at("target")})}}
                       .dump();
      if (!config) throw std::runtime_error("could not write the SSH terminal configuration");
    }

    auto session = std::make_shared<Session>();
    // Ownership of the claimed slot moves to the session here, whose
    // `slot_held` flag makes every later release path idempotent.
    slot.arm(session);
    session->server_id = server_id;
    session->username = payload.at("target").value("username", std::string());
    session->host = payload.at("target").value("host", std::string());
    session->cols = cols;
    session->rows = rows;
    session->config_path = config_path;
    try {
      spawn(session, binary, config_path, payload.at("credentials"));
    } catch (...) {
      std::error_code error;
      std::filesystem::remove(config_path, error);
      throw;
    }
    // Registration and thread start are one failure unit: if either throws, the
    // child that was already spawned must be stopped and the slot released here
    // rather than leaking a process the manager no longer tracks.
    try {
      {
        std::lock_guard lock(mutex);
        sessions[session_id] = session;
      }
      session->reader = std::thread([this, session] { reader_loop(session); });
    } catch (...) {
      {
        std::lock_guard lock(mutex);
        sessions.erase(session_id);
      }
      // `stop_session` reaps the spawned process and releases the slot; the
      // guard's release is then a no-op.
      stop_session(session);
      throw;
    }
    slot.dismiss();
    return session_id;
  }

  void spawn(const std::shared_ptr<Session>& session, const std::filesystem::path& binary,
             const std::filesystem::path& config_path, const nlohmann::json& credentials) {
#ifdef _WIN32
    SECURITY_ATTRIBUTES attributes{sizeof(SECURITY_ATTRIBUTES), nullptr, TRUE};
    HANDLE child_stdin = nullptr, child_stdout = nullptr, parent_stdin = nullptr, parent_stdout = nullptr;
    if (!CreatePipe(&child_stdin, &parent_stdin, &attributes, 0) ||
        !CreatePipe(&parent_stdout, &child_stdout, &attributes, 0)) {
      throw std::runtime_error("the desktop host could not create the terminal pipes");
    }
    SetHandleInformation(parent_stdin, HANDLE_FLAG_INHERIT, 0);
    SetHandleInformation(parent_stdout, HANDLE_FLAG_INHERIT, 0);

    std::wstring command_line = quote_argument(binary.wstring());
    command_line.push_back(L' ');
    command_line += quote_argument(L"--config");
    command_line.push_back(L' ');
    command_line += quote_argument(config_path.wstring());
    command_line.push_back(L' ');
    command_line += quote_argument(L"--terminal");
    command_line.push_back(L' ');
    command_line += quote_argument(L"--cols");
    command_line.push_back(L' ');
    command_line += quote_argument(std::to_wstring(session->cols));
    command_line.push_back(L' ');
    command_line += quote_argument(L"--rows");
    command_line.push_back(L' ');
    command_line += quote_argument(std::to_wstring(session->rows));
    std::vector<std::wstring> environment;
    for (const auto* name : {"SystemRoot", "WINDIR", "PATH", "TEMP", "TMP"}) {
      const auto needed = GetEnvironmentVariableW(to_wide(name).c_str(), nullptr, 0);
      if (needed == 0) continue;
      std::wstring value(static_cast<std::size_t>(needed), L'\0');
      const auto written = GetEnvironmentVariableW(to_wide(name).c_str(), value.data(), needed);
      if (written == 0 || written >= needed) continue;
      value.resize(written);
      environment.push_back(std::wstring(to_wide(name)) + L"=" + value);
    }
    for (auto item = credentials.begin(); item != credentials.end(); ++item) {
      environment.push_back(to_wide(item.key()) + L"=" + to_wide(item.value().get<std::string>()));
    }
    std::sort(environment.begin(), environment.end());
    std::wstring environment_block;
    for (const auto& entry : environment) {
      environment_block += entry;
      environment_block.push_back(L'\0');
    }
    environment_block.push_back(L'\0');

    STARTUPINFOW startup{};
    startup.cb = sizeof(startup);
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = child_stdin;
    startup.hStdOutput = child_stdout;
    startup.hStdError = child_stdout;
    PROCESS_INFORMATION info{};
    std::wstring mutable_command = command_line;
    if (!CreateProcessW(binary.wstring().c_str(), mutable_command.data(), nullptr, nullptr,
                        TRUE, CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
                        environment_block.data(), nullptr, &startup, &info)) {
      CloseHandle(child_stdin);
      CloseHandle(child_stdout);
      CloseHandle(parent_stdin);
      CloseHandle(parent_stdout);
      throw std::runtime_error("could not start the packaged C++ SSH terminal process");
    }
    CloseHandle(child_stdin);
    CloseHandle(child_stdout);
    CloseHandle(info.hThread);
    session->process = info.hProcess;
    session->stdin_write = parent_stdin;
    session->stdout_read = parent_stdout;
#else
    int child_stdin[2] = {-1, -1}, child_stdout[2] = {-1, -1};
    if (pipe(child_stdin) != 0 || pipe(child_stdout) != 0) {
      throw std::runtime_error("the desktop host could not create the terminal pipes");
    }
    const auto child = fork();
    if (child < 0) throw std::runtime_error("the desktop host could not fork the terminal process");
    if (child == 0) {
      dup2(child_stdin[0], STDIN_FILENO);
      dup2(child_stdout[1], STDOUT_FILENO);
      dup2(child_stdout[1], STDERR_FILENO);
      ::close(child_stdin[0]);
      ::close(child_stdin[1]);
      ::close(child_stdout[0]);
      ::close(child_stdout[1]);
      for (auto item = credentials.begin(); item != credentials.end(); ++item) {
        setenv(item.key().c_str(), item.value().get<std::string>().c_str(), 1);
      }
      std::vector<std::string> arguments = {binary.string(),
                                            "--config",
                                            config_path.string(),
                                            "--terminal",
                                            "--cols",
                                            std::to_string(session->cols),
                                            "--rows",
                                            std::to_string(session->rows)};
      std::vector<char*> argv;
      for (auto& argument : arguments) argv.push_back(argument.data());
      argv.push_back(nullptr);
      execve(binary.c_str(), argv.data(), environ);
      _exit(127);
    }
    ::close(child_stdin[0]);
    ::close(child_stdout[1]);
    session->pid = child;
    session->stdin_write = child_stdin[1];
    session->stdout_read = child_stdout[0];
#endif
  }
};

TerminalManager::TerminalManager(DesktopDatabase* database, const CredentialStore* credentials)
    : impl_(std::make_unique<Impl>()) {
  impl_->database = database;
  impl_->credentials = credentials;
}

TerminalManager::~TerminalManager() = default;

void TerminalManager::set_event_sink(EventSink sink) {
  std::lock_guard lock(impl_->mutex);
  impl_->sink = std::move(sink);
}

std::string TerminalManager::open(const std::string& server_id, int cols, int rows) {
  return impl_->open_session(server_id, cols, rows);
}

bool TerminalManager::write(const std::string& session_id, const std::string& data) {
  std::shared_ptr<Impl::Session> session;
  {
    std::lock_guard lock(impl_->mutex);
    const auto found = impl_->sessions.find(session_id);
    if (found == impl_->sessions.end()) return false;
    session = found->second;
  }
  return impl_->write_frame(session, {{"type", "write"}, {"data", data}});
}

bool TerminalManager::resize(const std::string& session_id, int cols, int rows) {
  if (cols < 1 || cols > 1000 || rows < 1 || rows > 1000) return false;
  std::shared_ptr<Impl::Session> session;
  {
    std::lock_guard lock(impl_->mutex);
    const auto found = impl_->sessions.find(session_id);
    if (found == impl_->sessions.end()) return false;
    session = found->second;
  }
  session->cols = cols;
  session->rows = rows;
  return impl_->write_frame(session, {{"type", "resize"}, {"columns", cols}, {"rows", rows}});
}

bool TerminalManager::close(const std::string& session_id) {
  std::shared_ptr<Impl::Session> session;
  {
    std::lock_guard lock(impl_->mutex);
    const auto found = impl_->sessions.find(session_id);
    if (found == impl_->sessions.end()) return false;
    session = found->second;
    impl_->sessions.erase(found);
  }
  impl_->stop_session(session);
  // A challenge belonging to a session that no longer exists can never be
  // completed; drop it instead of leaving an answerable id pointing at a dead PTY.
  {
    std::lock_guard lock(impl_->mutex);
    impl_->challenges.forget_session(session_id);
  }
  return true;
}

bool TerminalManager::respond_auth(const std::string& auth_id,
                                   const std::vector<std::string>& responses, std::string& error) {
  // The service requires the answer to carry exactly as many strings as the
  // challenge had prompts, and marks the session invalid otherwise — so the count
  // is checked here, before anything reaches the child.
  AuthChallenges::Entry entry;
  {
    std::lock_guard lock(impl_->mutex);
    const auto taken =
        impl_->challenges.take(auth_id, responses.size(), std::chrono::steady_clock::now(), error);
    if (!taken.has_value()) return false;
    entry = *taken;
  }
  std::shared_ptr<Impl::Session> session;
  {
    std::lock_guard lock(impl_->mutex);
    const auto found = impl_->sessions.find(entry.session_id);
    if (found == impl_->sessions.end() || found->second->closed.load()) {
      error = "the terminal session that asked for credentials has ended";
      return false;
    }
    session = found->second;
  }
  if (!impl_->write_frame(session, {{"type", "auth_response"}, {"responses", responses}})) {
    error = "the authentication answer could not be delivered to the terminal";
    return false;
  }
  return true;
}

bool TerminalManager::cancel_auth(const std::string& auth_id, std::string& error) {
  std::string session_id;
  {
    std::lock_guard lock(impl_->mutex);
    impl_->challenges.sweep(std::chrono::steady_clock::now());
    if (!impl_->challenges.cancel(auth_id, session_id)) {
      error = "no authentication challenge is waiting for that id";
      return false;
    }
  }
  // Cancelling must not leave the terminal hanging in the authentication phase.
  (void)close(session_id);
  return true;
}

}  // namespace yukinal::desktop_host

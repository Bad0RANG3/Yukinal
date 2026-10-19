#include "yukinal/desktop_host/sftp_transfer_backend.hpp"

#include "yukinal/desktop_host/credential_store.hpp"
#include "yukinal/desktop_host/database.hpp"
#include "yukinal/desktop_host/desktop_target.hpp"
#include "yukinal/desktop_host/scoped_environment.hpp"

#include <algorithm>
#include <cctype>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <memory>
#include <mutex>
#include <optional>
#include <random>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

#if defined(_WIN32)
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

#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
#include "yukinal/config.hpp"
#include "yukinal/ssh.hpp"
#endif

namespace yukinal::desktop_host {
namespace {

// One newline-delimited JSON frame from the worker never exceeds 48 KiB (a 32 KiB
// base64 data chunk is ~43.7 KiB). A longer line is treated as invalid framing.
constexpr std::size_t kMaxStreamFrameBytes = 48 * 1024;
constexpr std::size_t kMaxStreamChunkBytes = 32 * 1024;
constexpr int kTransferOperationTimeoutSeconds = 10;

// ── Base64 (standard alphabet, padded) ───────────────────────────────────────

char base64_alphabet(std::size_t index) {
  static constexpr char kAlphabet[] =
      "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  return kAlphabet[index & 0x3F];
}

int base64_value(char character) {
  if (character >= 'A' && character <= 'Z') return character - 'A';
  if (character >= 'a' && character <= 'z') return character - 'a' + 26;
  if (character >= '0' && character <= '9') return character - '0' + 52;
  if (character == '+') return 62;
  if (character == '/') return 63;
  return -1;
}

std::string base64_encode(const char* data, std::size_t size) {
  std::string encoded;
  encoded.reserve((size + 2) / 3 * 4);
  std::size_t index = 0;
  while (index + 3 <= size) {
    const auto first = static_cast<unsigned int>(static_cast<unsigned char>(data[index]));
    const auto second = static_cast<unsigned int>(static_cast<unsigned char>(data[index + 1]));
    const auto third = static_cast<unsigned int>(static_cast<unsigned char>(data[index + 2]));
    const unsigned int block = (first << 16) | (second << 8) | third;
    encoded.push_back(base64_alphabet(block >> 18));
    encoded.push_back(base64_alphabet(block >> 12));
    encoded.push_back(base64_alphabet(block >> 6));
    encoded.push_back(base64_alphabet(block));
    index += 3;
  }
  const std::size_t remaining = size - index;
  if (remaining == 1) {
    const auto first = static_cast<unsigned int>(static_cast<unsigned char>(data[index]));
    const unsigned int block = first << 16;
    encoded.push_back(base64_alphabet(block >> 18));
    encoded.push_back(base64_alphabet(block >> 12));
    encoded.push_back('=');
    encoded.push_back('=');
  } else if (remaining == 2) {
    const auto first = static_cast<unsigned int>(static_cast<unsigned char>(data[index]));
    const auto second = static_cast<unsigned int>(static_cast<unsigned char>(data[index + 1]));
    const unsigned int block = (first << 16) | (second << 8);
    encoded.push_back(base64_alphabet(block >> 18));
    encoded.push_back(base64_alphabet(block >> 12));
    encoded.push_back(base64_alphabet(block >> 6));
    encoded.push_back('=');
  }
  return encoded;
}

// Strict standard-base64 decode. Rejects a non-multiple-of-four length, illegal
// characters and non-canonical padding, matching the worker's own decoder.
bool base64_decode(const std::string& encoded, std::vector<char>* decoded) {
  if (decoded == nullptr || encoded.empty() || encoded.size() % 4 != 0) return false;
  const std::size_t padding = encoded.ends_with("==") ? 2 : encoded.ends_with("=") ? 1 : 0;
  for (std::size_t index = 0; index < encoded.size(); ++index) {
    const bool is_padding = encoded[index] == '=';
    if ((is_padding && (padding == 0 || index < encoded.size() - padding)) ||
        (!is_padding && base64_value(encoded[index]) < 0)) {
      return false;
    }
  }
  if (padding == 2 && (base64_value(encoded[encoded.size() - 3]) & 0x0F) != 0) return false;
  if (padding == 1 && (base64_value(encoded[encoded.size() - 2]) & 0x03) != 0) return false;
  decoded->clear();
  decoded->reserve(encoded.size() / 4 * 3 - padding);
  for (std::size_t index = 0; index < encoded.size(); index += 4) {
    const unsigned int first = static_cast<unsigned int>(base64_value(encoded[index]));
    const unsigned int second = static_cast<unsigned int>(base64_value(encoded[index + 1]));
    const unsigned int third = encoded[index + 2] == '='
                                   ? 0U
                                   : static_cast<unsigned int>(base64_value(encoded[index + 2]));
    const unsigned int fourth = encoded[index + 3] == '='
                                    ? 0U
                                    : static_cast<unsigned int>(base64_value(encoded[index + 3]));
    const unsigned int block = (first << 18) | (second << 12) | (third << 6) | fourth;
    decoded->push_back(static_cast<char>((block >> 16) & 0xFF));
    if (encoded[index + 2] != '=') decoded->push_back(static_cast<char>((block >> 8) & 0xFF));
    if (encoded[index + 3] != '=') decoded->push_back(static_cast<char>(block & 0xFF));
  }
  return true;
}

// ── Frame helpers ────────────────────────────────────────────────────────────

std::string json_string(const nlohmann::json& object, const char* key,
                        const std::string& fallback) {
  const auto found = object.find(key);
  if (found == object.end() || !found->is_string()) return fallback;
  return found->get<std::string>();
}

int json_exit_code(const nlohmann::json& object) {
  const auto found = object.find("exit_code");
  if (found == object.end() || !found->is_number_integer()) return -1;
  return found->get<int>();
}

// Parses one worker line. Returns `std::nullopt` when the line is empty. Throws
// `TransferError` when it is not a JSON object with a string `type`, so both the
// reader and the writer report the same framing failure.
nlohmann::json parse_worker_line(const std::string& line) {
  if (line.size() > kMaxStreamFrameBytes) {
    throw TransferError(TransferErrorCode::Remote, "the SFTP worker returned an oversized frame");
  }
  nlohmann::json message;
  try {
    message = nlohmann::json::parse(line);
  } catch (const nlohmann::json::exception&) {
    throw TransferError(TransferErrorCode::Remote, "the SFTP worker returned invalid framing");
  }
  if (!message.is_object() || !message.contains("type") || !message.at("type").is_string()) {
    throw TransferError(TransferErrorCode::Remote, "the SFTP worker returned an unexpected event");
  }
  return message;
}

// Consumes the mandatory `ready` handshake before any transfer byte moves.
void consume_ready_handshake(SftpStreamChannel& channel) {
  const auto line = channel.read_line();
  if (!line.has_value()) {
    throw TransferError(TransferErrorCode::Remote, "the SFTP worker exited during startup");
  }
  const auto message = parse_worker_line(*line);
  if (message.at("type").get<std::string>() == "ready") return;
  throw TransferError(
      TransferErrorCode::Remote,
      json_string(message, "message", "the SFTP worker could not start"));
}

// ── Streaming reader ─────────────────────────────────────────────────────────

class SftpStreamReader : public TransferReader {
 public:
  explicit SftpStreamReader(std::shared_ptr<SftpStreamChannel> channel)
      : channel_(std::move(channel)) {}

  std::size_t read(char* data, std::size_t size) override {
    if (finished_) return 0;
    if (pending_offset_ < pending_.size()) return copy_out(data, size);
    for (;;) {
      const auto line = channel_->read_line();
      if (!line.has_value()) {
        throw TransferError(TransferErrorCode::Remote,
                            "the SFTP download ended without a final event");
      }
      const auto message = parse_worker_line(*line);
      const auto type = message.at("type").get<std::string>();
      if (type == "data") {
        std::vector<char> bytes;
        const auto found = message.find("data");
        if (found == message.end() || !found->is_string() ||
            !base64_decode(found->get<std::string>(), &bytes) || bytes.empty() ||
            bytes.size() > kMaxStreamChunkBytes) {
          throw TransferError(TransferErrorCode::Remote,
                              "the SFTP download returned an invalid data chunk");
        }
        pending_ = std::move(bytes);
        pending_offset_ = 0;
        return copy_out(data, size);
      }
      if (type == "closed") {
        if (json_exit_code(message) != 0) {
          throw TransferError(TransferErrorCode::Remote,
                              "the SFTP download worker did not close cleanly");
        }
        if (channel_->wait() != 0) {
          throw TransferError(TransferErrorCode::Remote,
                              "the SFTP download worker did not exit cleanly");
        }
        finished_ = true;
        return 0;
      }
      if (type == "error") {
        throw TransferError(TransferErrorCode::Remote,
                            json_string(message, "message", "the SFTP download failed"));
      }
      throw TransferError(TransferErrorCode::Remote,
                          "the SFTP download returned an unexpected event");
    }
  }

 private:
  std::size_t copy_out(char* data, std::size_t size) {
    const auto count = std::min(size, pending_.size() - pending_offset_);
    if (count > 0) std::memcpy(data, pending_.data() + pending_offset_, count);
    pending_offset_ += count;
    if (pending_offset_ == pending_.size()) {
      pending_.clear();
      pending_offset_ = 0;
    }
    return count;
  }

  std::shared_ptr<SftpStreamChannel> channel_;
  std::vector<char> pending_;
  std::size_t pending_offset_ = 0;
  bool finished_ = false;
};

// ── Streaming writer ─────────────────────────────────────────────────────────

class SftpStreamWriter : public TransferWriter {
 public:
  explicit SftpStreamWriter(std::shared_ptr<SftpStreamChannel> channel)
      : channel_(std::move(channel)) {}

  void write(const char* data, std::size_t size) override {
    if (finished_) {
      throw TransferError(TransferErrorCode::Remote, "the SFTP upload is already closed");
    }
    std::size_t offset = 0;
    while (offset < size) {
      const auto count = std::min<std::size_t>(size - offset, kMaxStreamChunkBytes);
      const nlohmann::json frame = {
          {"type", "data"},
          {"data", base64_encode(data + offset, count)},
      };
      channel_->write_line(frame.dump());
      offset += count;
    }
  }

  void finish() override {
    if (finished_) return;
    finished_ = true;
    channel_->write_line(nlohmann::json{{"type", "close"}}.dump());
    const auto line = channel_->read_line();
    if (!line.has_value()) {
      throw TransferError(TransferErrorCode::Remote,
                          "the SFTP upload did not finish before the worker exited");
    }
    const auto message = parse_worker_line(*line);
    const auto type = message.at("type").get<std::string>();
    if (type == "error") {
      throw TransferError(TransferErrorCode::Remote,
                          json_string(message, "message", "the SFTP upload failed"));
    }
    if (type != "closed" || json_exit_code(message) != 0) {
      throw TransferError(TransferErrorCode::Remote,
                          "the SFTP upload returned an unexpected event");
    }
    channel_->close_write();
    if (channel_->wait() != 0) {
      throw TransferError(TransferErrorCode::Remote,
                          "the SFTP upload worker did not exit cleanly");
    }
  }

 private:
  std::shared_ptr<SftpStreamChannel> channel_;
  bool finished_ = false;
};

#if defined(YUKINAL_DESKTOP_HOST_WITH_SSH_CORE)

// ── Worker process channel ───────────────────────────────────────────────────
// Owns one spawned `yukinal-ssh-service` streaming worker and its temporary
// one-target config file. Lines flow as newline-delimited JSON in both
// directions; the destructor closes the pipes, reaps the process and removes the
// config so a cancelled transfer never leaks a worker or a secret file.

class ProcessSftpStreamChannel : public SftpStreamChannel {
 public:
#if defined(_WIN32)
  ProcessSftpStreamChannel(std::filesystem::path config_path, HANDLE process,
                           HANDLE stdin_write, HANDLE stdout_read)
      : config_path_(std::move(config_path)),
        process_(process),
        stdin_write_(stdin_write),
        stdout_read_(stdout_read) {}
#else
  ProcessSftpStreamChannel(std::filesystem::path config_path, pid_t pid, int stdin_write,
                           int stdout_read)
      : config_path_(std::move(config_path)),
        pid_(pid),
        stdin_write_(stdin_write),
        stdout_read_(stdout_read) {}
#endif

  ~ProcessSftpStreamChannel() override { release(); }

  void write_line(const std::string& line) override {
    const std::string payload = line + "\n";
    std::lock_guard lock(mutex_);
    if (write_closed_) {
      throw TransferError(TransferErrorCode::Remote, "the SFTP worker input is closed");
    }
#if defined(_WIN32)
    if (stdin_write_ == nullptr) {
      throw TransferError(TransferErrorCode::Remote, "the SFTP worker input is unavailable");
    }
    DWORD written = 0;
    std::size_t offset = 0;
    while (offset < payload.size()) {
      const auto remaining = static_cast<DWORD>(
          std::min<std::size_t>(payload.size() - offset, 1 << 20));
      if (!WriteFile(stdin_write_, payload.data() + offset, remaining, &written, nullptr) ||
          written == 0) {
        throw TransferError(TransferErrorCode::Remote,
                            "could not write to the SFTP worker");
      }
      offset += written;
    }
#else
    if (stdin_write_ < 0) {
      throw TransferError(TransferErrorCode::Remote, "the SFTP worker input is unavailable");
    }
    std::size_t offset = 0;
    while (offset < payload.size()) {
      const auto written = ::write(stdin_write_, payload.data() + offset, payload.size() - offset);
      if (written < 0) {
        if (errno == EINTR) continue;
        throw TransferError(TransferErrorCode::Remote, "could not write to the SFTP worker");
      }
      offset += static_cast<std::size_t>(written);
    }
#endif
  }

  void close_write() override {
    std::lock_guard lock(mutex_);
    if (write_closed_) return;
    write_closed_ = true;
#if defined(_WIN32)
    if (stdin_write_ != nullptr) {
      CloseHandle(stdin_write_);
      stdin_write_ = nullptr;
    }
#else
    if (stdin_write_ >= 0) {
      close(stdin_write_);
      stdin_write_ = -1;
    }
#endif
  }

  std::optional<std::string> read_line() override {
    std::lock_guard lock(mutex_);
    for (;;) {
      const auto newline = buffer_.find('\n');
      if (newline != std::string::npos) {
        auto line = buffer_.substr(0, newline);
        buffer_.erase(0, newline + 1);
        if (!line.empty() && line.back() == '\r') line.pop_back();
        return line;
      }
      if (buffer_.size() > kMaxStreamFrameBytes) {
        auto line = std::move(buffer_);
        buffer_.clear();
        return line;
      }
      if (!read_chunk_locked()) {
        if (!buffer_.empty()) {
          auto line = std::move(buffer_);
          buffer_.clear();
          return line;
        }
        return std::nullopt;
      }
    }
  }

  int wait() override {
#if defined(_WIN32)
    HANDLE process = nullptr;
    {
      std::lock_guard lock(mutex_);
      process = process_;
    }
    if (process == nullptr) return exit_code_;
    if (WaitForSingleObject(process, 5000) != WAIT_OBJECT_0) {
      TerminateProcess(process, 1);
      WaitForSingleObject(process, 5000);
      std::lock_guard lock(mutex_);
      exit_code_ = 1;
      return 1;
    }
    DWORD code = 1;
    if (!GetExitCodeProcess(process, &code)) code = 1;
    std::lock_guard lock(mutex_);
    exit_code_ = static_cast<int>(code);
    return exit_code_;
#else
    std::lock_guard lock(mutex_);
    reap_locked(5000);
    return exit_code_;
#endif
  }

 private:
  bool read_chunk_locked() {
    char buffer[16 * 1024];
#if defined(_WIN32)
    if (stdout_read_ == nullptr) return false;
    DWORD read = 0;
    if (!ReadFile(stdout_read_, buffer, sizeof(buffer), &read, nullptr) || read == 0) return false;
    buffer_.append(buffer, buffer + read);
    return true;
#else
    if (stdout_read_ < 0) return false;
    const auto read = ::read(stdout_read_, buffer, sizeof(buffer));
    if (read <= 0) return false;
    buffer_.append(buffer, buffer + read);
    return true;
#endif
  }

#if !defined(_WIN32)
  // Waits up to `timeout_ms` for the worker, then terminates it. Caches the exit
  // status so a second wait (reader then destructor) is idempotent.
  void reap_locked(int timeout_ms) {
    if (pid_ < 0) return;
    const int attempts = timeout_ms / 50;
    for (int attempt = 0; attempt < attempts; ++attempt) {
      int status = 0;
      if (waitpid(pid_, &status, WNOHANG) == pid_) {
        exit_code_ = WIFEXITED(status) ? WEXITSTATUS(status) : 1;
        pid_ = -1;
        return;
      }
      usleep(50 * 1000);
    }
    kill(pid_, SIGTERM);
    int status = 0;
    ::waitpid(pid_, &status, 0);
    exit_code_ = WIFEXITED(status) ? WEXITSTATUS(status) : 1;
    pid_ = -1;
  }
#endif

  void release() {
    close_write();
#if defined(_WIN32)
    if (process_ != nullptr) {
      if (WaitForSingleObject(process_, 2000) != WAIT_OBJECT_0) {
        TerminateProcess(process_, 1);
        WaitForSingleObject(process_, 2000);
      }
      CloseHandle(process_);
      std::lock_guard lock(mutex_);
      process_ = nullptr;
    }
    {
      std::lock_guard lock(mutex_);
      if (stdout_read_ != nullptr) {
        CloseHandle(stdout_read_);
        stdout_read_ = nullptr;
      }
    }
#else
    {
      std::lock_guard lock(mutex_);
      reap_locked(2000);
    }
    if (stdout_read_ >= 0) {
      close(stdout_read_);
      stdout_read_ = -1;
    }
#endif
    std::error_code error;
    if (!config_path_.empty()) std::filesystem::remove(config_path_, error);
  }

  std::filesystem::path config_path_;
  std::mutex mutex_;
  std::string buffer_;
  bool write_closed_ = false;
  int exit_code_ = 1;
#if defined(_WIN32)
  HANDLE process_ = nullptr;
  HANDLE stdin_write_ = nullptr;
  HANDLE stdout_read_ = nullptr;
#else
  pid_t pid_ = -1;
  int stdin_write_ = -1;
  int stdout_read_ = -1;
#endif
};

#if defined(_WIN32)
std::wstring to_wide(const std::string& value) {
  if (value.empty()) return {};
  const auto length = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                                          static_cast<int>(value.size()), nullptr, 0);
  if (length <= 0) {
    throw TransferError(TransferErrorCode::Remote, "an SFTP worker argument is not valid UTF-8");
  }
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
#endif

std::filesystem::path resolve_ssh_service_binary() {
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

std::string new_stream_nonce() {
  std::random_device device;
  static constexpr char kHex[] = "0123456789abcdef";
  std::string nonce;
  nonce.reserve(32);
  for (int index = 0; index < 16; ++index) {
    const auto byte = static_cast<unsigned>(device() & 0xFFu);
    nonce.push_back(kHex[byte >> 4]);
    nonce.push_back(kHex[byte & 0x0Fu]);
  }
  return nonce;
}

// Writes a one-target config the streaming worker reads, matching the terminal
// worker's `{listen_port, api_token_env, targets}` shape. The config carries only
// metadata; every secret is supplied through the child environment instead.
std::filesystem::path write_stream_config(const nlohmann::json& target) {
  const auto config_path = std::filesystem::temp_directory_path() /
                           ("yukinal-sftp-stream-" + new_stream_nonce() + ".json");
  std::ofstream config(config_path, std::ios::binary | std::ios::trunc);
  config << nlohmann::json{{"listen_port", 8765},
                           {"api_token_env", "YUKINAL_SSH_API_TOKEN"},
                           {"targets", nlohmann::json::array({target})}}
                    .dump();
  if (!config) {
    throw TransferError(TransferErrorCode::Remote,
                        "could not write the SFTP transfer configuration");
  }
  return config_path;
}

std::shared_ptr<SftpStreamChannel> spawn_process_sftp_stream_channel(
    const std::filesystem::path& binary, const std::filesystem::path& config_path,
    const std::string& mode, const std::string& path,
    const std::vector<std::pair<std::string, std::string>>& credentials) {
#if defined(_WIN32)
  SECURITY_ATTRIBUTES attributes{sizeof(SECURITY_ATTRIBUTES), nullptr, TRUE};
  HANDLE child_stdin = nullptr, child_stdout = nullptr, parent_stdin = nullptr,
         parent_stdout = nullptr;
  if (!CreatePipe(&child_stdin, &parent_stdin, &attributes, 0) ||
      !CreatePipe(&parent_stdout, &child_stdout, &attributes, 0)) {
    throw TransferError(TransferErrorCode::Remote,
                        "the desktop host could not create the SFTP stream pipes");
  }
  SetHandleInformation(parent_stdin, HANDLE_FLAG_INHERIT, 0);
  SetHandleInformation(parent_stdout, HANDLE_FLAG_INHERIT, 0);

  std::wstring command_line = quote_argument(binary.wstring());
  const auto append = [&command_line](const std::wstring& part) {
    command_line.push_back(L' ');
    command_line += quote_argument(part);
  };
  append(L"--config");
  append(config_path.wstring());
  append(to_wide(mode));
  append(L"--path");
  append(to_wide(path));

  std::vector<std::wstring> environment;
  for (const auto* name : {"SystemRoot", "WINDIR", "PATH", "TEMP", "TMP"}) {
    const auto wide_name = to_wide(name);
    const auto needed = GetEnvironmentVariableW(wide_name.c_str(), nullptr, 0);
    if (needed == 0) continue;
    std::wstring value(static_cast<std::size_t>(needed), L'\0');
    const auto written = GetEnvironmentVariableW(wide_name.c_str(), value.data(), needed);
    if (written == 0 || written >= needed) continue;
    value.resize(written);
    environment.push_back(wide_name + L"=" + value);
  }
  for (const auto& [name, value] : credentials) {
    environment.push_back(to_wide(name) + L"=" + to_wide(value));
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
  if (!CreateProcessW(binary.wstring().c_str(), mutable_command.data(), nullptr, nullptr, TRUE,
                      CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT, environment_block.data(),
                      nullptr, &startup, &info)) {
    CloseHandle(child_stdin);
    CloseHandle(child_stdout);
    CloseHandle(parent_stdin);
    CloseHandle(parent_stdout);
    throw TransferError(TransferErrorCode::Remote,
                        "could not start the packaged C++ SFTP worker");
  }
  CloseHandle(child_stdin);
  CloseHandle(child_stdout);
  CloseHandle(info.hThread);
  return std::make_shared<ProcessSftpStreamChannel>(config_path, info.hProcess, parent_stdin,
                                                    parent_stdout);
#else
  int child_stdin[2] = {-1, -1}, child_stdout[2] = {-1, -1};
  if (pipe(child_stdin) != 0 || pipe(child_stdout) != 0) {
    throw TransferError(TransferErrorCode::Remote,
                        "the desktop host could not create the SFTP stream pipes");
  }
  const auto child = fork();
  if (child < 0) {
    throw TransferError(TransferErrorCode::Remote,
                        "the desktop host could not fork the SFTP worker");
  }
  if (child == 0) {
    dup2(child_stdin[0], STDIN_FILENO);
    dup2(child_stdout[1], STDOUT_FILENO);
    dup2(child_stdout[1], STDERR_FILENO);
    close(child_stdin[0]);
    close(child_stdin[1]);
    close(child_stdout[0]);
    close(child_stdout[1]);
    for (const auto& [name, value] : credentials) {
      setenv(name.c_str(), value.c_str(), 1);
    }
    std::vector<std::string> arguments = {binary.string(), "--config", config_path.string(),
                                          mode, "--path", path};
    std::vector<char*> argv;
    for (auto& argument : arguments) argv.push_back(argument.data());
    argv.push_back(nullptr);
    execve(binary.c_str(), argv.data(), environ);
    _exit(127);
  }
  close(child_stdin[0]);
  close(child_stdout[1]);
  return std::make_shared<ProcessSftpStreamChannel>(config_path, child, child_stdin[1],
                                                    child_stdout[0]);
#endif
}

// ── Production remote-operation seam ─────────────────────────────────────────

std::vector<std::pair<std::string, std::string>> credential_values(
    const nlohmann::json& payload) {
  std::vector<std::pair<std::string, std::string>> values;
  const auto& credentials = payload.at("credentials");
  for (auto item = credentials.begin(); item != credentials.end(); ++item) {
    values.emplace_back(item.key(), item.value().get<std::string>());
  }
  return values;
}

yukinal::ssh_service::TargetConfig build_target_config(const nlohmann::json& payload) {
  const auto& target = payload.at("target");
  const auto target_id = target.at("target_id").get<std::string>();
  const nlohmann::json document = {
      {"listen_port", 8765},
      {"api_token_env", "YUKINAL_SSH_API_TOKEN"},
      {"targets", nlohmann::json::array({target})},
  };
  return yukinal::ssh_service::parse_config(document).targets.at(target_id);
}

RemoteEntryKind parse_entry_kind(const std::string& kind) {
  if (kind == "file") return RemoteEntryKind::File;
  if (kind == "directory") return RemoteEntryKind::Directory;
  if (kind == "symlink") return RemoteEntryKind::Symlink;
  if (kind == "other") return RemoteEntryKind::Other;
  throw TransferError(TransferErrorCode::Remote,
                      "the C++ SSH service returned an invalid SFTP entry type");
}

class SshServiceSftpRemoteOps : public SftpRemoteOps {
 public:
  SshServiceSftpRemoteOps(DesktopDatabase* database, const CredentialStore* credentials)
      : database_(database), credentials_(credentials) {}

  void ensure_supported(const std::string& server_id) override {
    if (!supports_ssh_service_target(*database_, server_id)) {
      throw TransferError(TransferErrorCode::Unsupported,
                          "the selected target is not supported by the C++ SSH service");
    }
  }

  std::optional<RemoteStat> lstat(const std::string& server_id,
                                  const std::string& path) override {
    const auto payload = resolve_payload(server_id);
    ScopedEnvironment environment(credential_values(payload));
    const auto result = yukinal::ssh_service::sftp_lstat_optional(
        build_target_config(payload), path, kTransferOperationTimeoutSeconds);
    const auto stdout_text = require_completed(result);
    nlohmann::json metadata;
    try {
      metadata = nlohmann::json::parse(stdout_text);
    } catch (const nlohmann::json::exception&) {
      throw TransferError(TransferErrorCode::Remote,
                          "the C++ SSH service returned invalid optional SFTP metadata");
    }
    if (!metadata.is_object() || !metadata.contains("exists") ||
        !metadata.at("exists").is_boolean()) {
      throw TransferError(TransferErrorCode::Remote,
                          "the C++ SSH service returned invalid optional SFTP metadata");
    }
    if (!metadata.at("exists").get<bool>()) {
      if (metadata.size() != 1) {
        throw TransferError(TransferErrorCode::Remote,
                            "the C++ SSH service returned inconsistent optional SFTP metadata");
      }
      return std::nullopt;
    }
    if (!metadata.contains("kind") || !metadata.at("kind").is_string() ||
        !metadata.contains("size") || !metadata.at("size").is_number_unsigned()) {
      throw TransferError(TransferErrorCode::Remote,
                          "the C++ SSH service returned incomplete optional SFTP metadata");
    }
    RemoteStat stat;
    stat.kind = parse_entry_kind(metadata.at("kind").get<std::string>());
    stat.size = metadata.at("size").get<std::uint64_t>();
    stat.modified = optional_modified(metadata);
    return stat;
  }

  std::vector<RemoteTransferEntry> list_dir(const std::string& server_id,
                                            const std::string& path) override {
    const auto payload = resolve_payload(server_id);
    ScopedEnvironment environment(credential_values(payload));
    const auto result = yukinal::ssh_service::sftp_list(
        build_target_config(payload), path, kTransferOperationTimeoutSeconds);
    nlohmann::json entries;
    try {
      entries = nlohmann::json::parse(require_completed(result));
    } catch (const nlohmann::json::exception&) {
      throw TransferError(TransferErrorCode::Remote,
                          "the C++ SSH service returned an invalid directory listing");
    }
    if (!entries.is_array()) {
      throw TransferError(TransferErrorCode::Remote,
                          "the C++ SSH service returned an invalid directory listing");
    }
    std::vector<RemoteTransferEntry> converted;
    converted.reserve(entries.size());
    for (const auto& entry : entries) {
      if (!entry.is_object() || !entry.contains("name") || !entry.at("name").is_string() ||
          !entry.contains("file_type") || !entry.at("file_type").is_string() ||
          !entry.contains("size") || !entry.at("size").is_number_unsigned()) {
        throw TransferError(TransferErrorCode::Remote,
                            "the C++ SSH service returned an invalid directory entry");
      }
      RemoteTransferEntry converted_entry;
      converted_entry.name = entry.at("name").get<std::string>();
      converted_entry.stat.kind = parse_entry_kind(entry.at("file_type").get<std::string>());
      converted_entry.stat.size = entry.at("size").get<std::uint64_t>();
      converted.push_back(std::move(converted_entry));
    }
    return converted;
  }

  std::unique_ptr<TransferReader> open_read(const std::string& server_id,
                                            const std::string& path) override {
    return make_sftp_stream_reader(spawn_stream(server_id, "--sftp-download", path));
  }

  std::unique_ptr<TransferWriter> create_exclusive(const std::string& server_id,
                                                   const std::string& path) override {
    return make_sftp_stream_writer(
        spawn_stream(server_id, "--sftp-upload-exclusive", path));
  }

  void create_dir(const std::string& server_id, const std::string& path) override {
    const auto payload = resolve_payload(server_id);
    ScopedEnvironment environment(credential_values(payload));
    const auto result = yukinal::ssh_service::sftp_create_dir(
        build_target_config(payload), path, kTransferOperationTimeoutSeconds);
    const auto stdout_text = require_completed(result);
    nlohmann::json metadata;
    try {
      metadata = nlohmann::json::parse(stdout_text);
    } catch (const nlohmann::json::exception&) {
      throw TransferError(TransferErrorCode::Remote,
                          "the C++ SSH service returned an invalid SFTP directory result");
    }
    if (!metadata.is_object() || metadata.value("created", false) != true) {
      throw TransferError(TransferErrorCode::Remote,
                          "the C++ SSH service did not confirm SFTP directory creation");
    }
  }

  void remove_file(const std::string& server_id, const std::string& path) override {
    const auto payload = resolve_payload(server_id);
    ScopedEnvironment environment(credential_values(payload));
    const auto result = yukinal::ssh_service::sftp_remove_file(
        build_target_config(payload), path, kTransferOperationTimeoutSeconds);
    const auto stdout_text = require_completed(result);
    nlohmann::json metadata;
    try {
      metadata = nlohmann::json::parse(stdout_text);
    } catch (const nlohmann::json::exception&) {
      throw TransferError(TransferErrorCode::Remote,
                          "the C++ SSH service returned an invalid SFTP removal result");
    }
    if (!metadata.is_object() || metadata.value("removed", false) != true) {
      throw TransferError(TransferErrorCode::Remote,
                          "the C++ SSH service did not confirm SFTP file removal");
    }
  }

  void publish_no_replace(const std::string& server_id, const std::string& source,
                          const std::string& destination) override {
    const auto payload = resolve_payload(server_id);
    ScopedEnvironment environment(credential_values(payload));
    const auto result = yukinal::ssh_service::sftp_publish_no_replace(
        build_target_config(payload), source, destination, kTransferOperationTimeoutSeconds);
    if (!result.error_code.empty() || !result.completed) {
      const std::string message =
          result.error_message.empty() ? result.error_code : result.error_message;
      if (result.error_code == "already_exists") {
        throw TransferError(TransferErrorCode::AlreadyExists, message);
      }
      if (result.error_code == "replace_unsupported") {
        throw TransferError(TransferErrorCode::Unsupported, message);
      }
      throw TransferError(TransferErrorCode::Remote, message);
    }
    nlohmann::json metadata;
    try {
      metadata = nlohmann::json::parse(result.stdout_text);
    } catch (const nlohmann::json::exception&) {
      throw TransferError(TransferErrorCode::Remote,
                          "the C++ SSH service returned invalid publication metadata");
    }
    if (!metadata.is_object() || metadata.value("published", false) != true) {
      throw TransferError(TransferErrorCode::Remote,
                          "the C++ SSH service did not confirm no-replace publication");
    }
  }

 private:
  nlohmann::json resolve_payload(const std::string& server_id) const {
    return resolve_ssh_service_target(*database_, *credentials_, server_id);
  }

  static std::string require_completed(const yukinal::ssh_service::ExecResult& result) {
    if (!result.error_code.empty() || !result.completed) {
      throw TransferError(TransferErrorCode::Remote, result.error_message.empty()
                                                         ? result.error_code
                                                         : result.error_message);
    }
    return result.stdout_text;
  }

  static std::optional<std::uint32_t> optional_modified(const nlohmann::json& metadata) {
    const auto found = metadata.find("modified");
    if (found == metadata.end() || !found->is_number_unsigned()) return std::nullopt;
    return static_cast<std::uint32_t>(found->get<std::uint64_t>());
  }

  std::shared_ptr<SftpStreamChannel> spawn_stream(const std::string& server_id,
                                                  const std::string& mode,
                                                  const std::string& path) {
    const auto payload = resolve_payload(server_id);
    const auto binary = resolve_ssh_service_binary();
    if (binary.empty() || !std::filesystem::is_regular_file(binary)) {
      throw TransferError(TransferErrorCode::Remote,
                          "the packaged C++ SSH service executable is unavailable");
    }
    const auto config_path = write_stream_config(payload.at("target"));
    return spawn_process_sftp_stream_channel(binary, config_path, mode, path,
                                             credential_values(payload));
  }

  DesktopDatabase* database_;
  const CredentialStore* credentials_;
};

#endif  // YUKINAL_DESKTOP_HOST_WITH_SSH_CORE

}  // namespace

std::unique_ptr<TransferReader> make_sftp_stream_reader(
    std::shared_ptr<SftpStreamChannel> channel) {
  consume_ready_handshake(*channel);
  return std::make_unique<SftpStreamReader>(std::move(channel));
}

std::unique_ptr<TransferWriter> make_sftp_stream_writer(
    std::shared_ptr<SftpStreamChannel> channel) {
  consume_ready_handshake(*channel);
  return std::make_unique<SftpStreamWriter>(std::move(channel));
}

SftpTransferBackend::SftpTransferBackend(std::shared_ptr<SftpRemoteOps> ops)
    : ops_(std::move(ops)) {}

std::optional<RemoteStat> SftpTransferBackend::lstat(const std::string& server_id,
                                                    const std::string& path) {
  ops_->ensure_supported(server_id);
  return ops_->lstat(server_id, path);
}

std::vector<RemoteTransferEntry> SftpTransferBackend::list_dir(const std::string& server_id,
                                                               const std::string& path) {
  ops_->ensure_supported(server_id);
  return ops_->list_dir(server_id, path);
}

std::unique_ptr<TransferReader> SftpTransferBackend::open_read(const std::string& server_id,
                                                               const std::string& path) {
  ops_->ensure_supported(server_id);
  return ops_->open_read(server_id, path);
}

std::unique_ptr<TransferWriter> SftpTransferBackend::create_exclusive(
    const std::string& server_id, const std::string& path) {
  ops_->ensure_supported(server_id);
  return ops_->create_exclusive(server_id, path);
}

void SftpTransferBackend::create_dir(const std::string& server_id, const std::string& path) {
  ops_->ensure_supported(server_id);
  ops_->create_dir(server_id, path);
}

void SftpTransferBackend::remove_file(const std::string& server_id, const std::string& path) {
  ops_->ensure_supported(server_id);
  ops_->remove_file(server_id, path);
}

void SftpTransferBackend::publish_no_replace(const std::string& server_id,
                                             const std::string& source,
                                             const std::string& destination) {
  ops_->ensure_supported(server_id);
  ops_->publish_no_replace(server_id, source, destination);
}

void SftpTransferBackend::rename_replace(const std::string& server_id, const std::string& source,
                                         const std::string& destination) {
  (void)server_id;
  (void)source;
  (void)destination;
  // Plain SFTP v3 has no explicit atomic replace, so the negotiated backend must
  // fail closed rather than emulate one with a copy.
  throw TransferError(TransferErrorCode::Unsupported,
                      "the negotiated SFTP backend has no explicit atomic replace operation");
}

void SftpTransferBackend::remove_recovered_staging(const std::string& server_id,
                                                   const std::string& path) {
  ops_->ensure_supported(server_id);
  const auto stat = ops_->lstat(server_id, path);
  if (!stat) return;
  if (stat->kind != RemoteEntryKind::File) {
    throw TransferError(TransferErrorCode::InvalidInput,
                        "recovered remote staging entry is not a regular file");
  }
  ops_->remove_file(server_id, path);
}

std::shared_ptr<SftpRemoteOps> make_ssh_service_sftp_remote_ops(
    DesktopDatabase* database, const CredentialStore* credentials) {
#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
  if (database == nullptr || credentials == nullptr) return nullptr;
  return std::make_shared<SshServiceSftpRemoteOps>(database, credentials);
#else
  (void)database;
  (void)credentials;
  return nullptr;
#endif
}

}  // namespace yukinal::desktop_host

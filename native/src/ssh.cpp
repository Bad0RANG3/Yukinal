#include "yukinal/ssh.hpp"
#include "yukinal/policy.hpp"

#include <algorithm>
#include <array>
#include <atomic>
#include <charconv>
#include <chrono>
#include <cerrno>
#include <cctype>
#include <climits>
#include <condition_variable>
#include <cstdlib>
#include <memory>
#include <mutex>
#include <string_view>
#include <thread>
#include <vector>

#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <winsock2.h>
#include <ws2tcpip.h>
#include <bcrypt.h>
#else
#include <arpa/inet.h>
#include <fcntl.h>
#include <netdb.h>
#include <sys/socket.h>
#include <unistd.h>
#include <openssl/evp.h>
#endif

#include "ssh_network.hpp"

#include <libssh2.h>
#include <libssh2_sftp.h>
#include <nlohmann/json.hpp>

namespace yukinal::ssh_service {
namespace {

#ifdef _WIN32
using Socket = SOCKET;
constexpr Socket kInvalidSocket = INVALID_SOCKET;
#else
using Socket = int;
constexpr Socket kInvalidSocket = -1;
#endif

constexpr std::size_t kMaxOutputBytes = 64 * 1024;
using Clock = std::chrono::steady_clock;

void close_socket(Socket socket) {
  if (socket == kInvalidSocket) return;
#ifdef _WIN32
  closesocket(socket);
#else
  close(socket);
#endif
}

int remaining_ms(Clock::time_point deadline) {
  const auto remaining = std::chrono::duration_cast<std::chrono::milliseconds>(deadline - Clock::now()).count();
  if (remaining <= 0) return 0;
  return static_cast<int>(std::min<std::int64_t>(remaining, INT_MAX));
}

bool set_nonblocking(Socket socket, bool nonblocking) {
#ifdef _WIN32
  u_long mode = nonblocking ? 1UL : 0UL;
  return ioctlsocket(socket, FIONBIO, &mode) == 0;
#else
  const int flags = fcntl(socket, F_GETFL, 0);
  if (flags < 0) return false;
  const int updated = nonblocking ? flags | O_NONBLOCK : flags & ~O_NONBLOCK;
  return fcntl(socket, F_SETFL, updated) == 0;
#endif
}

bool socket_would_block() {
#ifdef _WIN32
  const int error = WSAGetLastError();
  return error == WSAEWOULDBLOCK || error == WSAEINPROGRESS || error == WSAEALREADY;
#else
  return errno == EINPROGRESS || errno == EWOULDBLOCK || errno == EAGAIN;
#endif
}

}  // namespace

namespace detail {
namespace {

constexpr std::size_t kMaxBackgroundResolvers = 4;

std::atomic_size_t& active_background_resolvers() {
  // Timed-out system DNS calls cannot be cancelled portably. Keep this counter
  // alive until process exit because a detached resolver may outlive main.
  static auto* active = new std::atomic_size_t{0};
  return *active;
}

struct AddressInfoState {
  ~AddressInfoState() {
    if (addresses) freeaddrinfo(addresses);
  }

  std::mutex mutex;
  std::condition_variable ready;
  bool complete = false;
  bool abandoned = false;
  int result_code = -1;
  addrinfo* addresses = nullptr;
};

bool reserve_resolver_slot() {
  std::size_t active = active_background_resolvers().load();
  while (active < kMaxBackgroundResolvers) {
    if (active_background_resolvers().compare_exchange_weak(active, active + 1)) return true;
  }
  return false;
}

}  // namespace

AddressResolution resolve_address_list(const std::string& host, const std::string& service,
                                       const addrinfo& hints,
                                       std::chrono::steady_clock::time_point deadline,
                                       addrinfo** addresses, GetAddrInfo resolver) {
  if (!addresses) return AddressResolution::failed;
  *addresses = nullptr;
  if (std::chrono::steady_clock::now() >= deadline) return AddressResolution::timed_out;

  if (!resolver) {
    resolver = [](const char* node, const char* service_name, const addrinfo* request,
                  addrinfo** result) {
      return ::getaddrinfo(node, service_name, request, result);
    };
  }

  // Numeric addresses can be resolved locally without waiting on DNS.
  addrinfo numeric_hints = hints;
  numeric_hints.ai_flags |= AI_NUMERICHOST;
  addrinfo* numeric_addresses = nullptr;
  const int numeric_result = resolver(host.c_str(), service.c_str(), &numeric_hints,
                                      &numeric_addresses);
  if (numeric_result == 0 && numeric_addresses) {
    if (std::chrono::steady_clock::now() >= deadline) {
      freeaddrinfo(numeric_addresses);
      return AddressResolution::timed_out;
    }
    *addresses = numeric_addresses;
    return AddressResolution::resolved;
  }
  if (numeric_addresses) freeaddrinfo(numeric_addresses);
  if (std::chrono::steady_clock::now() >= deadline) return AddressResolution::timed_out;
  if (!reserve_resolver_slot()) return AddressResolution::busy;

  std::shared_ptr<AddressInfoState> state;
  try {
    state = std::make_shared<AddressInfoState>();
    // The platform resolver has no portable cancellation API. A detached
    // worker lets this operation return at its deadline; the four-slot cap
    // bounds workers that remain in an uninterruptible system call.
    std::thread worker([state, host, service, hints, resolver = std::move(resolver)]() mutable {
      addrinfo* resolved = nullptr;
      int result_code = -1;
      try {
        result_code = resolver(host.c_str(), service.c_str(), &hints, &resolved);
      } catch (...) {
        result_code = -1;
      }

      bool release_result = false;
      {
        std::lock_guard lock(state->mutex);
        if (state->abandoned) {
          release_result = true;
        } else {
          state->result_code = result_code;
          state->addresses = resolved;
          state->complete = true;
        }
      }
      if (release_result && resolved) freeaddrinfo(resolved);
      active_background_resolvers().fetch_sub(1);
      state->ready.notify_one();
    });
    worker.detach();
  } catch (...) {
    active_background_resolvers().fetch_sub(1);
    return AddressResolution::unavailable;
  }

  std::unique_lock lock(state->mutex);
  if (!state->ready.wait_until(lock, deadline, [&state] { return state->complete; })) {
    state->abandoned = true;
    return AddressResolution::timed_out;
  }
  if (std::chrono::steady_clock::now() >= deadline) {
    state->abandoned = true;
    return AddressResolution::timed_out;
  }
  if (state->result_code != 0 || !state->addresses) return AddressResolution::failed;
  *addresses = state->addresses;
  state->addresses = nullptr;
  return AddressResolution::resolved;
}

}  // namespace detail

namespace {

enum class ConnectFailure {
  none,
  connection_failed,
  timeout,
  resolver_busy,
  resolver_unavailable,
};

struct SocketConnection {
  Socket socket = kInvalidSocket;
  ConnectFailure failure = ConnectFailure::connection_failed;
};

bool wait_connect(Socket socket, int timeout_ms) {
  fd_set write_set;
  FD_ZERO(&write_set);
  FD_SET(socket, &write_set);
  timeval timeout{};
  timeout.tv_sec = timeout_ms / 1000;
  timeout.tv_usec = (timeout_ms % 1000) * 1000;
#ifdef _WIN32
  const int ready = select(0, nullptr, &write_set, nullptr, &timeout);
#else
  const int ready = select(socket + 1, nullptr, &write_set, nullptr, &timeout);
#endif
  if (ready <= 0) return false;

  int error = 0;
#ifdef _WIN32
  int length = sizeof(error);
#else
  socklen_t length = sizeof(error);
#endif
  return getsockopt(socket, SOL_SOCKET, SO_ERROR, reinterpret_cast<char*>(&error), &length) == 0 && error == 0;
}

SocketConnection connect_socket(const TargetConfig& target, Clock::time_point deadline) {
#ifdef _WIN32
  static std::once_flag winsock_once;
  static int winsock_result = 0;
  std::call_once(winsock_once, [] {
    WSADATA data{};
    winsock_result = WSAStartup(MAKEWORD(2, 2), &data);
  });
  if (winsock_result != 0) return {};
#endif

  addrinfo hints{};
  hints.ai_family = AF_UNSPEC;
  hints.ai_socktype = SOCK_STREAM;
  addrinfo* addresses = nullptr;
  const std::string port_text = std::to_string(target.port);
  const auto resolution = detail::resolve_address_list(target.host, port_text, hints, deadline,
                                                        &addresses);
  if (resolution != detail::AddressResolution::resolved || !addresses) {
    if (resolution == detail::AddressResolution::timed_out) return {kInvalidSocket, ConnectFailure::timeout};
    if (resolution == detail::AddressResolution::busy) return {kInvalidSocket, ConnectFailure::resolver_busy};
    if (resolution == detail::AddressResolution::unavailable) return {kInvalidSocket, ConnectFailure::resolver_unavailable};
    return {};
  }

  Socket connected = kInvalidSocket;
  for (addrinfo* address = addresses; address != nullptr; address = address->ai_next) {
    const int time_left = remaining_ms(deadline);
    if (time_left <= 0) break;

    Socket candidate = socket(address->ai_family, address->ai_socktype, address->ai_protocol);
    if (candidate == kInvalidSocket) continue;
    if (!set_nonblocking(candidate, true)) {
      close_socket(candidate);
      continue;
    }

#ifdef _WIN32
    const int connect_result = connect(candidate, address->ai_addr, static_cast<int>(address->ai_addrlen));
#else
    const int connect_result = connect(candidate, address->ai_addr, address->ai_addrlen);
#endif
    if (connect_result == 0 || (socket_would_block() && wait_connect(candidate, time_left))) {
      if (set_nonblocking(candidate, false)) connected = candidate;
      if (connected != kInvalidSocket) break;
    }
    close_socket(candidate);
  }
  freeaddrinfo(addresses);
  if (connected != kInvalidSocket) return {connected, ConnectFailure::none};
  return {kInvalidSocket, remaining_ms(deadline) == 0 ? ConnectFailure::timeout
                                                       : ConnectFailure::connection_failed};
}

bool wait_for_ssh(Socket socket, LIBSSH2_SESSION* session, Clock::time_point deadline) {
  const int time_left = remaining_ms(deadline);
  if (time_left <= 0) return false;
  int directions = libssh2_session_block_directions(session);
  if (directions == 0) directions = LIBSSH2_SESSION_BLOCK_INBOUND;
  fd_set read_set;
  fd_set write_set;
  FD_ZERO(&read_set);
  FD_ZERO(&write_set);
  if (directions & LIBSSH2_SESSION_BLOCK_INBOUND) FD_SET(socket, &read_set);
  if (directions & LIBSSH2_SESSION_BLOCK_OUTBOUND) FD_SET(socket, &write_set);
  timeval timeout{};
  timeout.tv_sec = time_left / 1000;
  timeout.tv_usec = (time_left % 1000) * 1000;
#ifdef _WIN32
  return select(0, &read_set, &write_set, nullptr, &timeout) > 0;
#else
  return select(socket + 1, &read_set, &write_set, nullptr, &timeout) > 0;
#endif
}

bool set_session_timeout(LIBSSH2_SESSION* session, Clock::time_point deadline) {
  const int time_left = remaining_ms(deadline);
  if (time_left <= 0) return false;
  libssh2_session_set_timeout(session, time_left);
  return true;
}

std::string read_environment(const std::string& name) {
  if (name.empty()) return {};
  const char* value = std::getenv(name.c_str());
  return value ? value : "";
}

class SshResources {
 public:
  ~SshResources() {
    if (sftp_handle_) libssh2_sftp_close_handle(sftp_handle_);
    if (sftp_) libssh2_sftp_shutdown(sftp_);
    if (channel_) {
      libssh2_channel_set_blocking(channel_, 0);
      libssh2_channel_close(channel_);
      libssh2_channel_free(channel_);
    }
    if (known_hosts_) libssh2_knownhost_free(known_hosts_);
    if (session_) {
      libssh2_session_set_blocking(session_, 0);
      libssh2_session_free(session_);
    }
    close_socket(socket_);
  }

  Socket socket_ = kInvalidSocket;
  LIBSSH2_SESSION* session_ = nullptr;
  LIBSSH2_KNOWNHOSTS* known_hosts_ = nullptr;
  LIBSSH2_SFTP* sftp_ = nullptr;
  LIBSSH2_SFTP_HANDLE* sftp_handle_ = nullptr;
  LIBSSH2_CHANNEL* channel_ = nullptr;
};

int known_host_key_type(int key_type) {
  switch (key_type) {
    case LIBSSH2_HOSTKEY_TYPE_RSA: return LIBSSH2_KNOWNHOST_KEY_SSHRSA;
    case LIBSSH2_HOSTKEY_TYPE_DSS: return LIBSSH2_KNOWNHOST_KEY_SSHDSS;
    case LIBSSH2_HOSTKEY_TYPE_ECDSA_256: return LIBSSH2_KNOWNHOST_KEY_ECDSA_256;
    case LIBSSH2_HOSTKEY_TYPE_ECDSA_384: return LIBSSH2_KNOWNHOST_KEY_ECDSA_384;
    case LIBSSH2_HOSTKEY_TYPE_ECDSA_521: return LIBSSH2_KNOWNHOST_KEY_ECDSA_521;
    case LIBSSH2_HOSTKEY_TYPE_ED25519: return LIBSSH2_KNOWNHOST_KEY_ED25519;
    default: return 0;
  }
}

void fail(ExecResult* result, const char* code, const char* message) {
  result->completed = false;
  result->error_code = code;
  result->error_message = message;
}

void wipe(std::string* value) {
  volatile char* data = value->empty() ? nullptr : value->data();
  for (std::size_t i = 0; i < value->size(); ++i) data[i] = '\0';
  value->clear();
}

struct DurationTracker {
  ExecResult& result;
  Clock::time_point started;
  ~DurationTracker() {
    result.duration_ms = std::chrono::duration_cast<std::chrono::milliseconds>(Clock::now() - started).count();
  }
};

bool append_output(std::string* output, const char* buffer, std::size_t length) {
  const std::size_t available = output->size() < kMaxOutputBytes ? kMaxOutputBytes - output->size() : 0;
  const std::size_t keep = std::min(available, length);
  output->append(buffer, keep);
  return keep == length;
}

std::string host_key_fingerprint(LIBSSH2_SESSION* session) {
  const char* hash = libssh2_hostkey_hash(session, LIBSSH2_HOSTKEY_HASH_SHA256);
  if (!hash) return {};

  constexpr char alphabet[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  constexpr std::size_t hash_length = 32;
  std::string encoded = "SHA256:";
  encoded.reserve(50);
  for (std::size_t i = 0; i < hash_length; i += 3) {
    const auto first = static_cast<unsigned char>(hash[i]);
    const auto second = i + 1 < hash_length ? static_cast<unsigned char>(hash[i + 1]) : 0;
    const auto third = i + 2 < hash_length ? static_cast<unsigned char>(hash[i + 2]) : 0;
    const unsigned int block = (static_cast<unsigned int>(first) << 16) |
                               (static_cast<unsigned int>(second) << 8) |
                               static_cast<unsigned int>(third);
    encoded.push_back(alphabet[(block >> 18) & 0x3f]);
    encoded.push_back(alphabet[(block >> 12) & 0x3f]);
    if (i + 1 < hash_length) encoded.push_back(alphabet[(block >> 6) & 0x3f]);
    if (i + 2 < hash_length) encoded.push_back(alphabet[block & 0x3f]);
  }
  return encoded;
}

bool connect_authenticated(const TargetConfig& target, Clock::time_point deadline,
                           SshResources* resources, ExecResult* result) {
  static std::once_flag libssh2_once;
  static int libssh2_init_result = 0;
  std::call_once(libssh2_once, [] { libssh2_init_result = libssh2_init(0); });
  if (libssh2_init_result != 0) {
    fail(result, "ssh_unavailable", "SSH library initialization failed");
    return false;
  }

  const SocketConnection connection = connect_socket(target, deadline);
  resources->socket_ = connection.socket;
  if (resources->socket_ == kInvalidSocket) {
    switch (connection.failure) {
      case ConnectFailure::timeout:
        fail(result, "timeout", "SSH connection timed out");
        break;
      case ConnectFailure::resolver_busy:
        fail(result, "resolver_busy", "DNS resolution capacity is temporarily exhausted");
        break;
      case ConnectFailure::resolver_unavailable:
        fail(result, "resolver_unavailable", "Could not start bounded DNS resolution");
        break;
      default:
        fail(result, "connection_failed", "Could not connect to the configured SSH target");
        break;
    }
    return false;
  }

  resources->session_ = libssh2_session_init();
  if (!resources->session_) {
    fail(result, "ssh_unavailable", "Could not create an SSH session");
    return false;
  }
  libssh2_session_set_blocking(resources->session_, 1);
  if (!set_session_timeout(resources->session_, deadline)) {
    fail(result, "timeout", "SSH connection timed out");
    return false;
  }
  if (libssh2_session_handshake(resources->session_, static_cast<libssh2_socket_t>(resources->socket_)) != 0) {
    const bool timed_out = remaining_ms(deadline) == 0 || libssh2_session_last_errno(resources->session_) == LIBSSH2_ERROR_TIMEOUT;
    fail(result, timed_out ? "timeout" : "ssh_handshake_failed",
         timed_out ? "SSH handshake timed out" : "SSH handshake failed");
    return false;
  }

  std::size_t host_key_length = 0;
  int host_key_type = 0;
  const char* host_key = libssh2_session_hostkey(resources->session_, &host_key_length, &host_key_type);
  const int known_key_type = known_host_key_type(host_key_type);
  if (!host_key || (target.host_key_fingerprint.empty() && known_key_type == 0)) {
    fail(result, "host_key_unverified", "The SSH server presented an unsupported host key");
    return false;
  }
  if (!target.host_key_fingerprint.empty()) {
    if (host_key_fingerprint(resources->session_) != target.host_key_fingerprint) {
      fail(result, "host_key_unverified", "The SSH host key differs from the configured fingerprint");
      return false;
    }
  } else {
    resources->known_hosts_ = libssh2_knownhost_init(resources->session_);
    if (!resources->known_hosts_ ||
        libssh2_knownhost_readfile(resources->known_hosts_, target.known_hosts_file.c_str(),
                                   LIBSSH2_KNOWNHOST_FILE_OPENSSH) < 0) {
      fail(result, "host_key_unverified", "Could not load the configured known-hosts file");
      return false;
    }
    const int known_host_result = libssh2_knownhost_checkp(
        resources->known_hosts_, target.host.c_str(), target.port, host_key, host_key_length,
        LIBSSH2_KNOWNHOST_TYPE_PLAIN | LIBSSH2_KNOWNHOST_KEYENC_RAW | known_key_type, nullptr);
    if (known_host_result != LIBSSH2_KNOWNHOST_CHECK_MATCH) {
      fail(result, "host_key_unverified",
           known_host_result == LIBSSH2_KNOWNHOST_CHECK_MISMATCH
               ? "The SSH host key differs from the configured known-hosts entry"
               : "The SSH host key is not present in the configured known-hosts file");
      return false;
    }
  }

  if (!set_session_timeout(resources->session_, deadline)) {
    fail(result, "timeout", "SSH authentication timed out");
    return false;
  }
  int auth_result = -1;
  if (!target.private_key_file.empty() || !target.private_key_env.empty()) {
    std::string passphrase = read_environment(target.passphrase_env);
    if (!target.private_key_env.empty()) {
      std::string private_key = read_environment(target.private_key_env);
      if (private_key.empty()) {
        fail(result, "credential_unavailable", "The configured SSH private key is not available");
        wipe(&passphrase);
        return false;
      }
      auth_result = libssh2_userauth_publickey_frommemory(
          resources->session_, target.username.c_str(), target.username.size(), nullptr, 0,
          private_key.data(), private_key.size(), passphrase.empty() ? nullptr : passphrase.c_str());
      wipe(&private_key);
    } else {
      auth_result = libssh2_userauth_publickey_fromfile(
          resources->session_, target.username.c_str(), nullptr, target.private_key_file.c_str(),
          passphrase.empty() ? nullptr : passphrase.c_str());
    }
    wipe(&passphrase);
  } else {
    std::string password = read_environment(target.password_env);
    if (password.empty()) {
      fail(result, "credential_unavailable", "The configured SSH credential is not available");
      return false;
    }
    auth_result = libssh2_userauth_password(resources->session_, target.username.c_str(), password.c_str());
    wipe(&password);
  }
  if (auth_result != 0) {
    const bool timed_out = remaining_ms(deadline) == 0 || libssh2_session_last_errno(resources->session_) == LIBSSH2_ERROR_TIMEOUT;
    fail(result, timed_out ? "timeout" : "authentication_failed",
         timed_out ? "SSH authentication timed out" : "SSH authentication failed");
    return false;
  }
  return true;
}

bool connect_sftp(const TargetConfig& target, Clock::time_point deadline,
                  SshResources* resources, ExecResult* result) {
  if (!connect_authenticated(target, deadline, resources, result)) return false;
  if (!set_session_timeout(resources->session_, deadline)) {
    fail(result, "timeout", "SFTP initialization timed out");
    return false;
  }
  resources->sftp_ = libssh2_sftp_init(resources->session_);
  if (!resources->sftp_) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(result, timed_out ? "timeout" : "sftp_error",
         timed_out ? "SFTP initialization timed out" : "Could not initialize SFTP");
    return false;
  }
  return true;
}

std::string base64_encode(const std::vector<unsigned char>& bytes) {
  static constexpr char alphabet[] =
      "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  std::string encoded;
  encoded.reserve(((bytes.size() + 2) / 3) * 4);
  for (std::size_t offset = 0; offset < bytes.size(); offset += 3) {
    const unsigned int first = bytes[offset];
    const unsigned int second = offset + 1 < bytes.size() ? bytes[offset + 1] : 0;
    const unsigned int third = offset + 2 < bytes.size() ? bytes[offset + 2] : 0;
    const unsigned int block = (first << 16) | (second << 8) | third;
    encoded.push_back(alphabet[(block >> 18) & 0x3f]);
    encoded.push_back(alphabet[(block >> 12) & 0x3f]);
    encoded.push_back(offset + 1 < bytes.size() ? alphabet[(block >> 6) & 0x3f] : '=');
    encoded.push_back(offset + 2 < bytes.size() ? alphabet[block & 0x3f] : '=');
  }
  return encoded;
}

}  // namespace

static ExecResult run_ssh_command(const TargetConfig& target, const std::string& command,
                                  int timeout_seconds) {
  ExecResult result;
  const auto started = Clock::now();
  const DurationTracker duration_tracker{result, started};
  if (timeout_seconds < 1 || timeout_seconds > 30) {
    fail(&result, "invalid_parameters", "The read-only SSH operation parameters are invalid");
    return result;
  }
  const auto deadline = started + std::chrono::seconds(timeout_seconds);
  SshResources resources;
  if (!connect_authenticated(target, deadline, &resources, &result)) return result;

  if (!set_session_timeout(resources.session_, deadline)) {
    fail(&result, "timeout", "SSH command timed out");
    return result;
  }
  resources.channel_ = libssh2_channel_open_session(resources.session_);
  if (!resources.channel_) {
    const bool timed_out = remaining_ms(deadline) == 0 || libssh2_session_last_errno(resources.session_) == LIBSSH2_ERROR_TIMEOUT;
    fail(&result, timed_out ? "timeout" : "ssh_error",
         timed_out ? "SSH command timed out" : "Could not open an SSH command channel");
    return result;
  }

  if (!set_session_timeout(resources.session_, deadline) ||
      libssh2_channel_process_startup(resources.channel_, "exec", 4, command.c_str(),
                                      static_cast<unsigned int>(command.size())) != 0) {
    const bool timed_out = remaining_ms(deadline) == 0 || libssh2_session_last_errno(resources.session_) == LIBSSH2_ERROR_TIMEOUT;
    fail(&result, timed_out ? "timeout" : "ssh_error",
         timed_out ? "Starting the SSH command timed out" : "Could not start the read-only SSH operation");
    return result;
  }

  libssh2_channel_set_blocking(resources.channel_, 0);
  std::array<char, 8192> buffer{};
  bool read_error = false;
  bool output_limit_exceeded = false;
  while (!libssh2_channel_eof(resources.channel_)) {
    if (remaining_ms(deadline) == 0) {
      fail(&result, "timeout", "SSH command timed out; the remote result is unknown");
      break;
    }
    bool made_progress = false;
    for (const int stream_id : {0, SSH_EXTENDED_DATA_STDERR}) {
      for (;;) {
        const auto count = libssh2_channel_read_ex(resources.channel_, stream_id, buffer.data(), buffer.size());
        if (count == LIBSSH2_ERROR_EAGAIN || count == 0) break;
        if (count < 0) {
          read_error = true;
          break;
        }
        made_progress = true;
        std::string& output = stream_id == 0 ? result.stdout_text : result.stderr_text;
        if (!append_output(&output, buffer.data(), static_cast<std::size_t>(count))) {
          result.output_truncated = true;
          output_limit_exceeded = true;
          break;
        }
      }
      if (read_error || output_limit_exceeded) break;
    }
    if (read_error) {
      fail(&result, "ssh_error", "Reading the SSH command result failed");
      break;
    }
    if (output_limit_exceeded) {
      fail(&result, "output_limit", "SSH output exceeded the configured 64 KiB per-stream limit");
      break;
    }
    if (!libssh2_channel_eof(resources.channel_) && !made_progress && !wait_for_ssh(resources.socket_, resources.session_, deadline)) {
      fail(&result, "timeout", "SSH command timed out; the remote result is unknown");
      break;
    }
  }

  if (result.error_code.empty()) {
    result.exit_code = libssh2_channel_get_exit_status(resources.channel_);
    if (result.exit_code < 0) {
      fail(&result, "result_unknown", "The SSH connection closed before an exit status was received");
    } else if (result.output_truncated) {
      fail(&result, "output_limit", "SSH output exceeded the configured 64 KiB per-stream limit");
    } else {
      result.completed = true;
    }
  }
  return result;
}

ExecResult service_status(const TargetConfig& target, const std::string& service,
                          int timeout_seconds) {
  if (!valid_service_name(service)) {
    ExecResult result;
    fail(&result, "invalid_parameters", "The read-only SSH operation parameters are invalid");
    return result;
  }
  return run_ssh_command(target, "systemctl is-active -- " + service, timeout_seconds);
}

ExecResult service_inventory(const TargetConfig& target, int timeout_seconds) {
  static constexpr char command[] =
      "if command -v systemctl >/dev/null 2>&1; then "
      "systemctl list-units --type=service --all --no-legend --no-pager --plain 2>/dev/null; "
      "if [ $? -eq 0 ]; then printf '\\n__YUKINAL_SOURCE__=systemd\\n'; exit 0; fi; fi; "
      "if command -v docker >/dev/null 2>&1; then "
      "docker ps -a --format '{{.Names}}\\t{{.State}}\\t{{.Status}}\\t{{.Image}}' 2>/dev/null; "
      "if [ $? -eq 0 ]; then printf '\\n__YUKINAL_SOURCE__=docker\\n'; exit 0; fi; fi; "
      "printf '__YUKINAL_SOURCE__=unavailable\\n'";
  return run_ssh_command(target, command, timeout_seconds);
}

ExecResult sftp_list(const TargetConfig& target, const std::string& path, int timeout_seconds) {
  ExecResult result;
  const auto started = Clock::now();
  const DurationTracker duration_tracker{result, started};
  if (path.empty() || path.size() > 4096 || path.find('\0') != std::string::npos ||
      timeout_seconds < 1 || timeout_seconds > 30) {
    fail(&result, "invalid_parameters", "The SFTP directory listing parameters are invalid");
    return result;
  }

  const auto deadline = started + std::chrono::seconds(timeout_seconds);
  SshResources resources;
  if (!connect_sftp(target, deadline, &resources, &result)) return result;
  resources.sftp_handle_ = libssh2_sftp_open_ex(
      resources.sftp_, path.data(), static_cast<unsigned int>(path.size()), 0, 0,
      LIBSSH2_SFTP_OPENDIR);
  if (!resources.sftp_handle_) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(&result, timed_out ? "timeout" : "sftp_error",
         timed_out ? "SFTP directory listing timed out" : "Could not open the remote directory");
    return result;
  }

  nlohmann::json entries = nlohmann::json::array();
  std::array<char, 4096> name{};
  for (;;) {
    if (!set_session_timeout(resources.session_, deadline)) {
      fail(&result, "timeout", "SFTP directory listing timed out");
      return result;
    }
    LIBSSH2_SFTP_ATTRIBUTES attributes{};
    const int count = libssh2_sftp_readdir_ex(resources.sftp_handle_, name.data(),
                                               static_cast<unsigned int>(name.size()), nullptr, 0,
                                               &attributes);
    if (count == 0) break;
    if (count < 0) {
      const bool timed_out = remaining_ms(deadline) == 0;
      fail(&result, timed_out ? "timeout" : "sftp_error",
           timed_out ? "SFTP directory listing timed out" : "Could not read the remote directory");
      return result;
    }
    const std::string entry_name(name.data(), static_cast<std::size_t>(count));
    std::string file_type = "other";
    if ((attributes.flags & LIBSSH2_SFTP_ATTR_PERMISSIONS) != 0) {
      switch (attributes.permissions & LIBSSH2_SFTP_S_IFMT) {
        case LIBSSH2_SFTP_S_IFDIR: file_type = "directory"; break;
        case LIBSSH2_SFTP_S_IFREG: file_type = "file"; break;
        case LIBSSH2_SFTP_S_IFLNK: file_type = "symlink"; break;
        default: break;
      }
    }
    const std::uint64_t size = (attributes.flags & LIBSSH2_SFTP_ATTR_SIZE) != 0
                                   ? attributes.filesize
                                   : 0;
    entries.push_back({{"name", entry_name}, {"file_type", file_type}, {"size", size}});
    if (entries.size() > 4096) {
      fail(&result, "output_limit", "The remote directory contains too many entries to list safely");
      return result;
    }
  }
  result.stdout_text = entries.dump();
  if (result.stdout_text.size() > 256 * 1024) {
    result.stdout_text.clear();
    fail(&result, "output_limit", "The remote directory listing exceeded the response size limit");
    return result;
  }
  result.exit_code = 0;
  result.completed = true;
  return result;
}

ExecResult sftp_stat(const TargetConfig& target, const std::string& path, int timeout_seconds) {
  ExecResult result;
  const auto started = Clock::now();
  const DurationTracker duration_tracker{result, started};
  if (path.empty() || path.size() > 4096 || path.find('\0') != std::string::npos ||
      timeout_seconds < 1 || timeout_seconds > 30) {
    fail(&result, "invalid_parameters", "The SFTP metadata request parameters are invalid");
    return result;
  }
  const auto deadline = started + std::chrono::seconds(timeout_seconds);
  SshResources resources;
  if (!connect_sftp(target, deadline, &resources, &result)) return result;
  if (!set_session_timeout(resources.session_, deadline)) {
    fail(&result, "timeout", "SFTP metadata request timed out");
    return result;
  }

  LIBSSH2_SFTP_ATTRIBUTES attributes{};
  if (libssh2_sftp_stat_ex(resources.sftp_, path.data(), static_cast<unsigned int>(path.size()),
                           LIBSSH2_SFTP_LSTAT, &attributes) != 0) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(&result, timed_out ? "timeout" : "sftp_error",
         timed_out ? "SFTP metadata request timed out" : "Could not read remote path metadata");
    return result;
  }

  std::string file_type = "other";
  if ((attributes.flags & LIBSSH2_SFTP_ATTR_PERMISSIONS) != 0) {
    switch (attributes.permissions & LIBSSH2_SFTP_S_IFMT) {
      case LIBSSH2_SFTP_S_IFDIR: file_type = "directory"; break;
      case LIBSSH2_SFTP_S_IFREG: file_type = "file"; break;
      case LIBSSH2_SFTP_S_IFLNK: file_type = "symlink"; break;
      default: break;
    }
  }
  nlohmann::json output = {
      {"kind", file_type},
      {"size", (attributes.flags & LIBSSH2_SFTP_ATTR_SIZE) != 0 ? attributes.filesize : 0},
      {"modified", (attributes.flags & LIBSSH2_SFTP_ATTR_ACMODTIME) != 0
                       ? nlohmann::json(attributes.mtime)
                       : nlohmann::json(nullptr)},
  };
  result.stdout_text = output.dump();
  result.exit_code = 0;
  result.completed = true;
  return result;
}

ExecResult sftp_read(const TargetConfig& target, const std::string& path,
                     std::uint32_t max_bytes, int timeout_seconds) {
  ExecResult result;
  const auto started = Clock::now();
  const DurationTracker duration_tracker{result, started};
  if (path.empty() || path.size() > 4096 || path.find('\0') != std::string::npos ||
      max_bytes > 1024 * 1024 || timeout_seconds < 1 || timeout_seconds > 30) {
    fail(&result, "invalid_parameters", "The bounded SFTP read parameters are invalid");
    return result;
  }
  const auto deadline = started + std::chrono::seconds(timeout_seconds);
  SshResources resources;
  if (!connect_sftp(target, deadline, &resources, &result)) return result;
  if (!set_session_timeout(resources.session_, deadline)) {
    fail(&result, "timeout", "SFTP file read timed out");
    return result;
  }
  resources.sftp_handle_ = libssh2_sftp_open_ex(
      resources.sftp_, path.data(), static_cast<unsigned int>(path.size()), LIBSSH2_FXF_READ, 0,
      LIBSSH2_SFTP_OPENFILE);
  if (!resources.sftp_handle_) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(&result, timed_out ? "timeout" : "sftp_error",
         timed_out ? "SFTP file read timed out" : "Could not open the remote file for reading");
    return result;
  }

  const std::size_t limit = static_cast<std::size_t>(max_bytes) + 1;
  std::vector<unsigned char> bytes;
  bytes.reserve(limit);
  std::array<char, 32 * 1024> buffer{};
  while (bytes.size() < limit) {
    if (!set_session_timeout(resources.session_, deadline)) {
      fail(&result, "timeout", "SFTP file read timed out");
      return result;
    }
    const std::size_t remaining = limit - bytes.size();
    const std::size_t requested = std::min(remaining, buffer.size());
    const auto count = libssh2_sftp_read(resources.sftp_handle_, buffer.data(), requested);
    if (count == 0) break;
    if (count < 0) {
      const bool timed_out = remaining_ms(deadline) == 0;
      fail(&result, timed_out ? "timeout" : "sftp_error",
           timed_out ? "SFTP file read timed out" : "Could not read the remote file");
      return result;
    }
    const auto read_count = static_cast<std::size_t>(count);
    bytes.insert(bytes.end(), reinterpret_cast<const unsigned char*>(buffer.data()),
                 reinterpret_cast<const unsigned char*>(buffer.data()) + read_count);
  }
  result.stdout_text = base64_encode(bytes);
  result.exit_code = 0;
  result.completed = true;
  return result;
}

namespace {

ExecResult sftp_write_impl(const TargetConfig& target, const std::string& path,
                           const std::vector<unsigned char>& content, bool exclusive,
                           int timeout_seconds) {
  ExecResult result;
  const auto started = Clock::now();
  const DurationTracker duration_tracker{result, started};
  const std::size_t maximum = exclusive ? 1024 * 1024 : 512 * 1024;
  if (path.empty() || path.size() > 4096 || path.find('\0') != std::string::npos ||
      content.size() > maximum || timeout_seconds < 1 || timeout_seconds > 30) {
    fail(&result, "invalid_parameters", "The bounded SFTP write parameters are invalid");
    return result;
  }

  const auto deadline = started + std::chrono::seconds(timeout_seconds);
  SshResources resources;
  if (!connect_sftp(target, deadline, &resources, &result)) return result;
  const long flags = LIBSSH2_FXF_WRITE | LIBSSH2_FXF_CREAT |
                     (exclusive ? LIBSSH2_FXF_EXCL : LIBSSH2_FXF_TRUNC);
  resources.sftp_handle_ = libssh2_sftp_open_ex(
      resources.sftp_, path.data(), static_cast<unsigned int>(path.size()), flags, 0644,
      LIBSSH2_SFTP_OPENFILE);
  if (!resources.sftp_handle_) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(&result, timed_out ? "timeout" : "sftp_error",
         timed_out ? "SFTP file write timed out" : "Could not open the remote file for writing");
    return result;
  }

  bool write_succeeded = true;
  for (std::size_t offset = 0; offset < content.size();) {
    if (!set_session_timeout(resources.session_, deadline)) {
      fail(&result, "timeout", "SFTP file write timed out");
      write_succeeded = false;
      break;
    }
    const auto count = libssh2_sftp_write(
        resources.sftp_handle_, reinterpret_cast<const char*>(content.data() + offset),
        content.size() - offset);
    if (count <= 0) {
      const bool timed_out = remaining_ms(deadline) == 0;
      fail(&result, timed_out ? "timeout" : "sftp_error",
           timed_out ? "SFTP file write timed out" : "Could not write the remote file");
      write_succeeded = false;
      break;
    }
    offset += static_cast<std::size_t>(count);
  }

  if (write_succeeded && exclusive) {
    if (!set_session_timeout(resources.session_, deadline) ||
        libssh2_sftp_fsync(resources.sftp_handle_) != 0) {
      const bool timed_out = remaining_ms(deadline) == 0;
      fail(&result, timed_out ? "timeout" : "sftp_error",
           timed_out ? "Syncing the SFTP recovery file timed out"
                     : "The SFTP server could not sync the recovery file");
      write_succeeded = false;
    }
  }

  if (!set_session_timeout(resources.session_, deadline)) {
    // Close a partially-created recovery file even when its main operation reached the deadline.
    // The cleanup grace is bounded and is not used to continue the requested write.
    libssh2_session_set_timeout(resources.session_, 1000);
  }
  if (resources.sftp_handle_) {
    LIBSSH2_SFTP_HANDLE* handle = resources.sftp_handle_;
    resources.sftp_handle_ = nullptr;
    if (libssh2_sftp_close_handle(handle) != 0 && write_succeeded) {
      fail(&result, remaining_ms(deadline) == 0 ? "timeout" : "sftp_error",
           remaining_ms(deadline) == 0 ? "Closing the SFTP file timed out"
                                       : "Could not close the remote file after writing");
      write_succeeded = false;
    }
  } else {
    write_succeeded = false;
    if (result.error_code.empty()) fail(&result, "timeout", "SFTP file write timed out");
  }

  if (!write_succeeded && exclusive) {
    if (!set_session_timeout(resources.session_, deadline)) libssh2_session_set_timeout(resources.session_, 1000);
    libssh2_sftp_unlink_ex(resources.sftp_, path.data(), static_cast<unsigned int>(path.size()));
    return result;
  }
  if (!write_succeeded) return result;

  result.stdout_text = nlohmann::json{{"bytes_written", content.size()}}.dump();
  result.exit_code = 0;
  result.completed = true;
  return result;
}

bool compute_sha256(const std::vector<unsigned char>& bytes,
                    std::array<unsigned char, 32>* digest) {
  if (!digest) return false;
#ifdef _WIN32
  BCRYPT_ALG_HANDLE algorithm = nullptr;
  BCRYPT_HASH_HANDLE hash = nullptr;
  DWORD object_size = 0;
  DWORD result_size = 0;
  std::vector<unsigned char> object;
  bool succeeded = BCryptOpenAlgorithmProvider(&algorithm, BCRYPT_SHA256_ALGORITHM, nullptr, 0) >= 0;
  if (succeeded) {
    succeeded = BCryptGetProperty(algorithm, BCRYPT_OBJECT_LENGTH,
                                  reinterpret_cast<PUCHAR>(&object_size), sizeof(object_size),
                                  &result_size, 0) >= 0 && object_size > 0;
  }
  if (succeeded) {
    object.resize(object_size);
    succeeded = BCryptCreateHash(algorithm, &hash, object.data(), object_size,
                                 nullptr, 0, 0) >= 0;
  }
  if (succeeded && !bytes.empty()) {
    succeeded = BCryptHashData(hash, const_cast<PUCHAR>(bytes.data()),
                               static_cast<ULONG>(bytes.size()), 0) >= 0;
  }
  if (succeeded) {
    succeeded = BCryptFinishHash(hash, digest->data(), static_cast<ULONG>(digest->size()), 0) >= 0;
  }
  if (hash) BCryptDestroyHash(hash);
  if (algorithm) BCryptCloseAlgorithmProvider(algorithm, 0);
  return succeeded;
#else
  unsigned int digest_length = 0;
  return EVP_Digest(bytes.data(), bytes.size(), digest->data(), &digest_length,
                    EVP_sha256(), nullptr) == 1 && digest_length == digest->size();
#endif
}

bool sftp_lstat(LIBSSH2_SFTP* sftp, const std::string& path,
                Clock::time_point deadline, LIBSSH2_SESSION* session,
                LIBSSH2_SFTP_ATTRIBUTES* attributes) {
  if (!set_session_timeout(session, deadline)) return false;
  *attributes = {};
  return libssh2_sftp_stat_ex(sftp, path.data(), static_cast<unsigned int>(path.size()),
                              LIBSSH2_SFTP_LSTAT, attributes) == 0;
}

bool is_regular_file(const LIBSSH2_SFTP_ATTRIBUTES& attributes) {
  return (attributes.flags & LIBSSH2_SFTP_ATTR_PERMISSIONS) != 0 &&
         LIBSSH2_SFTP_S_ISREG(attributes.permissions);
}

std::optional<std::uint32_t> modified_time(const LIBSSH2_SFTP_ATTRIBUTES& attributes) {
  if ((attributes.flags & LIBSSH2_SFTP_ATTR_ACMODTIME) == 0) return std::nullopt;
  return attributes.mtime;
}

LIBSSH2_SFTP_ATTRIBUTES preserved_attributes(const LIBSSH2_SFTP_ATTRIBUTES& source) {
  LIBSSH2_SFTP_ATTRIBUTES output{};
  if ((source.flags & LIBSSH2_SFTP_ATTR_PERMISSIONS) != 0) {
    output.flags |= LIBSSH2_SFTP_ATTR_PERMISSIONS;
    output.permissions = source.permissions;
  }
  if ((source.flags & LIBSSH2_SFTP_ATTR_UIDGID) != 0) {
    output.flags |= LIBSSH2_SFTP_ATTR_UIDGID;
    output.uid = source.uid;
    output.gid = source.gid;
  }
  if ((source.flags & LIBSSH2_SFTP_ATTR_ACMODTIME) != 0) {
    output.flags |= LIBSSH2_SFTP_ATTR_ACMODTIME;
    output.atime = source.atime;
    output.mtime = source.mtime;
  }
  return output;
}

std::vector<std::string> unpreserved_metadata(const LIBSSH2_SFTP_ATTRIBUTES& source,
                                              const LIBSSH2_SFTP_ATTRIBUTES& staged) {
  std::vector<std::string> missing;
  if ((source.flags & LIBSSH2_SFTP_ATTR_PERMISSIONS) != 0 &&
      ((staged.flags & LIBSSH2_SFTP_ATTR_PERMISSIONS) == 0 ||
       source.permissions != staged.permissions)) {
    missing.emplace_back("mode");
  }
  if ((source.flags & LIBSSH2_SFTP_ATTR_ACMODTIME) != 0 &&
      ((staged.flags & LIBSSH2_SFTP_ATTR_ACMODTIME) == 0 || source.mtime != staged.mtime)) {
    missing.emplace_back("mtime");
  }
  if ((source.flags & LIBSSH2_SFTP_ATTR_UIDGID) != 0) {
    if ((staged.flags & LIBSSH2_SFTP_ATTR_UIDGID) == 0 || source.uid != staged.uid) {
      missing.emplace_back("owner");
    }
    if ((staged.flags & LIBSSH2_SFTP_ATTR_UIDGID) == 0 || source.gid != staged.gid) {
      missing.emplace_back("group");
    }
  }
  return missing;
}

class StagingFileCleanup {
 public:
  StagingFileCleanup(SshResources* resources, std::string path)
      : resources_(resources), path_(std::move(path)) {}
  ~StagingFileCleanup() {
    if (!resources_ || !resources_->sftp_ || !created_ || published_) return;
    libssh2_session_set_timeout(resources_->session_, 1000);
    libssh2_sftp_unlink_ex(resources_->sftp_, path_.data(),
                           static_cast<unsigned int>(path_.size()));
  }
  void mark_created() { created_ = true; }
  void mark_published() { published_ = true; }

 private:
  SshResources* resources_;
  std::string path_;
  bool created_ = false;
  bool published_ = false;
};

ExecResult sftp_replace_guarded_impl(const TargetConfig& target, const ExecRequest& request) {
  ExecResult result;
  const auto started = Clock::now();
  const DurationTracker duration_tracker{result, started};
  if (request.path.empty() || request.path.size() > 4096 || request.path.front() != '/' ||
      request.path.find('\0') != std::string::npos || request.path.find_first_of("\r\n") != std::string::npos ||
      request.expected_size > 1024 * 1024 || request.content.size() > 1024 * 1024 ||
      request.timeout_seconds < 1 || request.timeout_seconds > 30) {
    fail(&result, "invalid_parameters", "The guarded SFTP replacement parameters are invalid");
    return result;
  }
  if (request.path == "/" || request.path.back() == '/') {
    fail(&result, "replace_unsupported", "A root or directory path cannot be replaced as a regular file");
    return result;
  }

  static std::atomic_uint64_t stage_sequence{0};
  const auto nonce = std::chrono::steady_clock::now().time_since_epoch().count() ^
                     static_cast<std::int64_t>(stage_sequence.fetch_add(1));
  const std::string stage_path = request.path + ".yukinal-stage-" + std::to_string(nonce);
  if (stage_path.size() > 4096) {
    fail(&result, "replace_unsupported", "The path is too long to place a sibling staging file safely");
    return result;
  }

  const auto deadline = started + std::chrono::seconds(request.timeout_seconds);
  SshResources resources;
  if (!connect_sftp(target, deadline, &resources, &result)) return result;
  LIBSSH2_SFTP_ATTRIBUTES source{};
  if (!sftp_lstat(resources.sftp_, request.path, deadline, resources.session_, &source)) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(&result, timed_out ? "timeout" : "sftp_error",
         timed_out ? "Reading the guarded SFTP target timed out" : "Could not inspect the guarded SFTP target");
    return result;
  }
  if (!is_regular_file(source)) {
    fail(&result, "replace_unsupported", "Only a regular file can be replaced atomically; symlinks and directories are refused");
    return result;
  }
  const std::uint64_t source_size = (source.flags & LIBSSH2_SFTP_ATTR_SIZE) != 0 ? source.filesize : 0;
  if (source_size != request.expected_size || modified_time(source) != request.expected_modified) {
    fail(&result, "concurrent_change", "The target size or mtime changed after it was read");
    return result;
  }

  StagingFileCleanup cleanup(&resources, stage_path);
  LIBSSH2_SFTP_ATTRIBUTES desired = preserved_attributes(source);
  const long create_mode = (source.flags & LIBSSH2_SFTP_ATTR_PERMISSIONS) != 0
                               ? static_cast<long>(source.permissions)
                               : static_cast<long>(0600);
  if (!set_session_timeout(resources.session_, deadline)) {
    fail(&result, "timeout", "Creating the guarded SFTP staging file timed out");
    return result;
  }
  resources.sftp_handle_ = libssh2_sftp_open_ex(
      resources.sftp_, stage_path.data(), static_cast<unsigned int>(stage_path.size()),
      LIBSSH2_FXF_WRITE | LIBSSH2_FXF_CREAT | LIBSSH2_FXF_EXCL, create_mode,
      LIBSSH2_SFTP_OPENFILE);
  if (!resources.sftp_handle_) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(&result, timed_out ? "timeout" : "replace_unsupported",
         timed_out ? "Creating the guarded SFTP staging file timed out"
                   : "The server refused an exclusive sibling staging file, so nothing was published");
    return result;
  }
  cleanup.mark_created();

  bool stage_ok = true;
  for (std::size_t offset = 0; offset < request.content.size();) {
    if (!set_session_timeout(resources.session_, deadline)) {
      fail(&result, "timeout", "Writing the guarded SFTP staging file timed out");
      stage_ok = false;
      break;
    }
    const auto count = libssh2_sftp_write(
        resources.sftp_handle_, reinterpret_cast<const char*>(request.content.data() + offset),
        request.content.size() - offset);
    if (count <= 0) {
      const bool timed_out = remaining_ms(deadline) == 0;
      fail(&result, timed_out ? "timeout" : "sftp_error",
           timed_out ? "Writing the guarded SFTP staging file timed out"
                     : "Could not write the guarded SFTP staging file");
      stage_ok = false;
      break;
    }
    offset += static_cast<std::size_t>(count);
  }

  if (stage_ok) {
    if (!set_session_timeout(resources.session_, deadline)) {
      fail(&result, "timeout", "Preserving SFTP file metadata timed out");
      stage_ok = false;
    } else {
      const bool handle_metadata_set = desired.flags == 0 ||
          libssh2_sftp_fsetstat(resources.sftp_handle_, &desired) == 0;
      if (!handle_metadata_set) {
        if (!set_session_timeout(resources.session_, deadline)) {
          fail(&result, "timeout", "Preserving SFTP file metadata timed out");
          stage_ok = false;
        } else {
          libssh2_sftp_setstat(resources.sftp_, stage_path.c_str(), &desired);
        }
      }
    }
  }
  if (stage_ok && (!set_session_timeout(resources.session_, deadline) ||
                   libssh2_sftp_fsync(resources.sftp_handle_) != 0)) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(&result, timed_out ? "timeout" : "sftp_error",
         timed_out ? "Syncing the guarded SFTP staging file timed out"
                   : "The SFTP server could not sync the guarded staging file");
    stage_ok = false;
  }
  if (resources.sftp_handle_) {
    LIBSSH2_SFTP_HANDLE* handle = resources.sftp_handle_;
    resources.sftp_handle_ = nullptr;
    if (!set_session_timeout(resources.session_, deadline)) libssh2_session_set_timeout(resources.session_, 1000);
    if (libssh2_sftp_close_handle(handle) != 0 && stage_ok) {
      const bool timed_out = remaining_ms(deadline) == 0;
      fail(&result, timed_out ? "timeout" : "sftp_error",
           timed_out ? "Closing the guarded SFTP staging file timed out"
                     : "Could not close the guarded SFTP staging file");
      stage_ok = false;
    }
  }
  if (!stage_ok) return result;

  LIBSSH2_SFTP_ATTRIBUTES staged{};
  if (!sftp_lstat(resources.sftp_, stage_path, deadline, resources.session_, &staged)) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(&result, timed_out ? "timeout" : "sftp_error",
         timed_out ? "Verifying the SFTP staging file timed out" : "Could not verify the SFTP staging file");
    return result;
  }
  result.missing_metadata = unpreserved_metadata(source, staged);
  if (!result.missing_metadata.empty()) {
    result.error_code = "metadata_not_preserved";
    result.error_message = "The remote server would not preserve the file's metadata; the edit was not published";
    return result;
  }

  LIBSSH2_SFTP_ATTRIBUTES before_read{};
  if (!sftp_lstat(resources.sftp_, request.path, deadline, resources.session_, &before_read)) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(&result, timed_out ? "timeout" : "sftp_error",
         timed_out ? "Rechecking the guarded SFTP target timed out" : "Could not recheck the guarded SFTP target");
    return result;
  }
  if (!is_regular_file(before_read) ||
      ((before_read.flags & LIBSSH2_SFTP_ATTR_SIZE) != 0 ? before_read.filesize : 0) != request.expected_size ||
      modified_time(before_read) != request.expected_modified) {
    fail(&result, "concurrent_change", "The target metadata changed while the replacement was staged");
    return result;
  }

  if (!set_session_timeout(resources.session_, deadline)) {
    fail(&result, "timeout", "Opening the target for guarded content verification timed out");
    return result;
  }
  LIBSSH2_SFTP_HANDLE* read_handle = libssh2_sftp_open_ex(
      resources.sftp_, request.path.data(), static_cast<unsigned int>(request.path.size()),
      LIBSSH2_FXF_READ, 0, LIBSSH2_SFTP_OPENFILE);
  if (!read_handle) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(&result, timed_out ? "timeout" : "sftp_error",
         timed_out ? "Opening the target for guarded content verification timed out"
                   : "Could not read the target for guarded content verification");
    return result;
  }
  std::vector<unsigned char> current;
  current.reserve(static_cast<std::size_t>(request.expected_size) + 1);
  std::array<char, 32 * 1024> buffer{};
  bool read_ok = true;
  while (current.size() <= request.expected_size) {
    if (!set_session_timeout(resources.session_, deadline)) {
      fail(&result, "timeout", "Reading the target for guarded content verification timed out");
      read_ok = false;
      break;
    }
    const std::size_t remaining = static_cast<std::size_t>(request.expected_size + 1 - current.size());
    const auto count = libssh2_sftp_read(read_handle, buffer.data(), std::min(remaining, buffer.size()));
    if (count == 0) break;
    if (count < 0) {
      const bool timed_out = remaining_ms(deadline) == 0;
      fail(&result, timed_out ? "timeout" : "sftp_error",
           timed_out ? "Reading the target for guarded content verification timed out"
                     : "Could not read the target for guarded content verification");
      read_ok = false;
      break;
    }
    current.insert(current.end(), reinterpret_cast<const unsigned char*>(buffer.data()),
                   reinterpret_cast<const unsigned char*>(buffer.data()) + static_cast<std::size_t>(count));
  }
  if (!set_session_timeout(resources.session_, deadline)) libssh2_session_set_timeout(resources.session_, 1000);
  const int read_close = libssh2_sftp_close_handle(read_handle);
  if (read_ok && read_close != 0) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(&result, timed_out ? "timeout" : "sftp_error",
         timed_out ? "Closing guarded content verification timed out"
                   : "Could not close guarded content verification");
    read_ok = false;
  }
  if (!read_ok) return result;
  if (current.size() != request.expected_size) {
    fail(&result, "concurrent_change", "The target size changed while its content was being verified");
    return result;
  }
  std::array<unsigned char, 32> current_digest{};
  if (!compute_sha256(current, &current_digest)) {
    fail(&result, "internal_error", "SHA-256 content verification is unavailable");
    return result;
  }
  if (current_digest != request.expected_digest) {
    fail(&result, "concurrent_change", "The target content changed while the replacement was staged");
    return result;
  }

  LIBSSH2_SFTP_ATTRIBUTES after_read{};
  if (!sftp_lstat(resources.sftp_, request.path, deadline, resources.session_, &after_read)) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(&result, timed_out ? "timeout" : "sftp_error",
         timed_out ? "Rechecking target metadata after content verification timed out"
                   : "Could not recheck target metadata after content verification");
    return result;
  }
  if (!is_regular_file(after_read) ||
      ((after_read.flags & LIBSSH2_SFTP_ATTR_SIZE) != 0 ? after_read.filesize : 0) != request.expected_size ||
      modified_time(after_read) != request.expected_modified) {
    fail(&result, "concurrent_change", "The target metadata changed during content verification");
    return result;
  }

  if (!set_session_timeout(resources.session_, deadline)) {
    fail(&result, "timeout", "Publishing the guarded SFTP replacement timed out");
    return result;
  }
  const int rename_result = libssh2_sftp_posix_rename_ex(
      resources.sftp_, stage_path.data(), stage_path.size(), request.path.data(), request.path.size());
  if (rename_result != 0) {
    const int session_error = libssh2_session_last_errno(resources.session_);
    const bool timed_out = remaining_ms(deadline) == 0 || session_error == LIBSSH2_ERROR_TIMEOUT;
    if (timed_out) {
      fail(&result, "timeout", "Publishing the guarded SFTP replacement timed out");
    } else if (session_error == LIBSSH2_FX_OP_UNSUPPORTED ||
               session_error == LIBSSH2_ERROR_SFTP_PROTOCOL) {
      fail(&result, "replace_unsupported",
           "The server refused safe atomic replacement, so the edit was not published");
    } else {
      fail(&result, "sftp_error", "The guarded SFTP replacement could not be confirmed");
    }
    return result;
  }
  cleanup.mark_published();

  LIBSSH2_SFTP_ATTRIBUTES published{};
  if (!sftp_lstat(resources.sftp_, request.path, deadline, resources.session_, &published)) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(&result, timed_out ? "timeout" : "sftp_error",
         timed_out ? "Verifying the published replacement timed out"
                   : "The replacement was published but its metadata could not be verified");
    return result;
  }
  if (!is_regular_file(published) ||
      ((published.flags & LIBSSH2_SFTP_ATTR_SIZE) != 0 ? published.filesize : 0) != request.content.size()) {
    fail(&result, "concurrent_change", "The published path does not contain the replacement that was staged");
    return result;
  }
  const auto published_modified = modified_time(published);
  result.stdout_text = nlohmann::json{
      {"size", (published.flags & LIBSSH2_SFTP_ATTR_SIZE) != 0 ? published.filesize : 0},
      {"modified", published_modified ? nlohmann::json(*published_modified) : nlohmann::json(nullptr)}}.dump();
  result.exit_code = 0;
  result.completed = true;
  return result;
}

std::string shell_single_quote(const std::string& value) {
  std::string quoted = "'";
  for (const char character : value) {
    if (character == '\'') {
      quoted += "'\\''";
    } else {
      quoted.push_back(character);
    }
  }
  quoted.push_back('\'');
  return quoted;
}

}  // namespace

ExecResult sftp_write(const TargetConfig& target, const std::string& path,
                      const std::vector<unsigned char>& content, int timeout_seconds) {
  return sftp_write_impl(target, path, content, false, timeout_seconds);
}

ExecResult sftp_create_exclusive(const TargetConfig& target, const std::string& path,
                                 const std::vector<unsigned char>& content, int timeout_seconds) {
  return sftp_write_impl(target, path, content, true, timeout_seconds);
}

ExecResult sftp_remove_file(const TargetConfig& target, const std::string& path,
                            int timeout_seconds) {
  ExecResult result;
  const auto started = Clock::now();
  const DurationTracker duration_tracker{result, started};
  if (path.empty() || path.size() > 4096 || path.find('\0') != std::string::npos ||
      timeout_seconds < 1 || timeout_seconds > 30) {
    fail(&result, "invalid_parameters", "The SFTP file removal parameters are invalid");
    return result;
  }
  const auto deadline = started + std::chrono::seconds(timeout_seconds);
  SshResources resources;
  if (!connect_sftp(target, deadline, &resources, &result)) return result;
  if (!set_session_timeout(resources.session_, deadline)) {
    fail(&result, "timeout", "SFTP file removal timed out");
    return result;
  }
  if (libssh2_sftp_unlink_ex(resources.sftp_, path.data(), static_cast<unsigned int>(path.size())) != 0) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(&result, timed_out ? "timeout" : "sftp_error",
         timed_out ? "SFTP file removal timed out" : "Could not remove the remote file");
    return result;
  }
  result.stdout_text = "{\"removed\":true}";
  result.exit_code = 0;
  result.completed = true;
  return result;
}

ExecResult sftp_replace_guarded(const TargetConfig& target, const ExecRequest& request) {
  return sftp_replace_guarded_impl(target, request);
}

ExecResult sftp_link_count(const TargetConfig& target, const std::string& path,
                           int timeout_seconds) {
  ExecResult result;
  const auto started = Clock::now();
  const DurationTracker duration_tracker{result, started};
  if (path.empty() || path.size() > 4096 || path.front() != '/' ||
      path.find('\0') != std::string::npos || path.find_first_of("\r\n") != std::string::npos ||
      timeout_seconds < 1 || timeout_seconds > 30) {
    fail(&result, "invalid_parameters", "The SFTP link-count path is invalid");
    return result;
  }
  const std::string quoted = shell_single_quote(path);
  ExecResult probe = run_ssh_command(
      target, "stat -c %h " + quoted + " 2>/dev/null || stat -f %l " + quoted + " 2>/dev/null",
      timeout_seconds);
  if (!probe.completed) return probe;

  std::string_view output(probe.stdout_text);
  while (!output.empty() && std::isspace(static_cast<unsigned char>(output.front()))) output.remove_prefix(1);
  while (!output.empty() && std::isspace(static_cast<unsigned char>(output.back()))) output.remove_suffix(1);
  std::uint64_t count = 0;
  const auto parsed = std::from_chars(output.data(), output.data() + output.size(), count);
  const bool valid = probe.exit_code == 0 && !output.empty() &&
                     parsed.ec == std::errc{} && parsed.ptr == output.data() + output.size();
  probe.stdout_text = nlohmann::json{{"link_count", valid ? nlohmann::json(count) : nlohmann::json(nullptr)}}.dump();
  probe.stderr_text.clear();
  probe.exit_code = 0;
  probe.completed = true;
  return probe;
}

ExecResult run_operation(const TargetConfig& target, const ExecRequest& request) {
  if (request.operation == "service_status") {
    return service_status(target, request.service, request.timeout_seconds);
  }
  if (request.operation == "service_inventory") {
    return service_inventory(target, request.timeout_seconds);
  }
  if (request.operation == "sftp_list") {
    return sftp_list(target, request.path, request.timeout_seconds);
  }
  if (request.operation == "sftp_stat") {
    return sftp_stat(target, request.path, request.timeout_seconds);
  }
  if (request.operation == "sftp_read") {
    return sftp_read(target, request.path, request.max_bytes, request.timeout_seconds);
  }
  if (request.operation == "sftp_write") {
    return sftp_write(target, request.path, request.content, request.timeout_seconds);
  }
  if (request.operation == "sftp_create_exclusive") {
    return sftp_create_exclusive(target, request.path, request.content, request.timeout_seconds);
  }
  if (request.operation == "sftp_remove_file") {
    return sftp_remove_file(target, request.path, request.timeout_seconds);
  }
  if (request.operation == "sftp_replace_guarded") {
    return sftp_replace_guarded(target, request);
  }
  if (request.operation == "sftp_link_count") {
    return sftp_link_count(target, request.path, request.timeout_seconds);
  }
  ExecResult result;
  fail(&result, "unsupported_operation", "The requested structured SSH operation is not enabled");
  return result;
}

}  // namespace yukinal::ssh_service

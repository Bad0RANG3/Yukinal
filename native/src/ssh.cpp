#include "yukinal/ssh.hpp"
#include "yukinal/policy.hpp"

#include <algorithm>
#include <array>
#include <atomic>
#include <chrono>
#include <cerrno>
#include <climits>
#include <condition_variable>
#include <cstdlib>
#include <memory>
#include <mutex>
#include <thread>

#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <winsock2.h>
#include <ws2tcpip.h>
#else
#include <arpa/inet.h>
#include <fcntl.h>
#include <netdb.h>
#include <sys/socket.h>
#include <unistd.h>
#endif

#include "ssh_network.hpp"

#include <libssh2.h>

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

}  // namespace

ExecResult service_status(const TargetConfig& target, const std::string& service,
                          int timeout_seconds) {
  ExecResult result;
  const auto started = Clock::now();
  const DurationTracker duration_tracker{result, started};
  if (!valid_service_name(service) || timeout_seconds < 1 || timeout_seconds > 30) {
    fail(&result, "invalid_parameters", "The read-only SSH operation parameters are invalid");
    return result;
  }
  const auto deadline = started + std::chrono::seconds(timeout_seconds);
  static std::once_flag libssh2_once;
  static int libssh2_init_result = 0;
  std::call_once(libssh2_once, [] { libssh2_init_result = libssh2_init(0); });
  if (libssh2_init_result != 0) {
    fail(&result, "ssh_unavailable", "SSH library initialization failed");
    return result;
  }

  SshResources resources;
  const SocketConnection connection = connect_socket(target, deadline);
  resources.socket_ = connection.socket;
  if (resources.socket_ == kInvalidSocket) {
    switch (connection.failure) {
      case ConnectFailure::timeout:
        fail(&result, "timeout", "SSH connection timed out");
        break;
      case ConnectFailure::resolver_busy:
        fail(&result, "resolver_busy", "DNS resolution capacity is temporarily exhausted");
        break;
      case ConnectFailure::resolver_unavailable:
        fail(&result, "resolver_unavailable", "Could not start bounded DNS resolution");
        break;
      default:
        fail(&result, "connection_failed", "Could not connect to the configured SSH target");
        break;
    }
    return result;
  }

  resources.session_ = libssh2_session_init();
  if (!resources.session_) {
    fail(&result, "ssh_unavailable", "Could not create an SSH session");
    return result;
  }
  libssh2_session_set_blocking(resources.session_, 1);
  if (!set_session_timeout(resources.session_, deadline)) {
    fail(&result, "timeout", "SSH connection timed out");
    return result;
  }
  if (libssh2_session_handshake(resources.session_, static_cast<libssh2_socket_t>(resources.socket_)) != 0) {
    const bool timed_out = remaining_ms(deadline) == 0 || libssh2_session_last_errno(resources.session_) == LIBSSH2_ERROR_TIMEOUT;
    fail(&result, timed_out ? "timeout" : "ssh_handshake_failed",
         timed_out ? "SSH handshake timed out" : "SSH handshake failed");
    return result;
  }

  resources.known_hosts_ = libssh2_knownhost_init(resources.session_);
  if (!resources.known_hosts_ ||
      libssh2_knownhost_readfile(resources.known_hosts_, target.known_hosts_file.c_str(),
                                 LIBSSH2_KNOWNHOST_FILE_OPENSSH) < 0) {
    fail(&result, "host_key_unverified", "Could not load the configured known-hosts file");
    return result;
  }
  std::size_t host_key_length = 0;
  int host_key_type = 0;
  const char* host_key = libssh2_session_hostkey(resources.session_, &host_key_length, &host_key_type);
  const int known_key_type = known_host_key_type(host_key_type);
  if (!host_key || known_key_type == 0) {
    fail(&result, "host_key_unverified", "The SSH server presented an unsupported host key");
    return result;
  }
  const int known_host_result = libssh2_knownhost_checkp(
      resources.known_hosts_, target.host.c_str(), target.port, host_key, host_key_length,
      LIBSSH2_KNOWNHOST_TYPE_PLAIN | LIBSSH2_KNOWNHOST_KEYENC_RAW | known_key_type, nullptr);
  if (known_host_result != LIBSSH2_KNOWNHOST_CHECK_MATCH) {
    fail(&result, "host_key_unverified",
         known_host_result == LIBSSH2_KNOWNHOST_CHECK_MISMATCH
             ? "The SSH host key differs from the configured known-hosts entry"
             : "The SSH host key is not present in the configured known-hosts file");
    return result;
  }

  if (!set_session_timeout(resources.session_, deadline)) {
    fail(&result, "timeout", "SSH authentication timed out");
    return result;
  }
  int auth_result = -1;
  if (!target.private_key_file.empty()) {
    std::string passphrase = read_environment(target.passphrase_env);
    auth_result = libssh2_userauth_publickey_fromfile(
        resources.session_, target.username.c_str(), nullptr, target.private_key_file.c_str(),
        passphrase.empty() ? nullptr : passphrase.c_str());
    wipe(&passphrase);
  } else {
    std::string password = read_environment(target.password_env);
    if (password.empty()) {
      fail(&result, "credential_unavailable", "The configured SSH credential is not available");
      return result;
    }
    auth_result = libssh2_userauth_password(resources.session_, target.username.c_str(), password.c_str());
    wipe(&password);
  }
  if (auth_result != 0) {
    const bool timed_out = remaining_ms(deadline) == 0 || libssh2_session_last_errno(resources.session_) == LIBSSH2_ERROR_TIMEOUT;
    fail(&result, timed_out ? "timeout" : "authentication_failed",
         timed_out ? "SSH authentication timed out" : "SSH authentication failed");
    return result;
  }

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

  // The service name is schema-validated before it reaches this fixed read-only command.
  const std::string command = "systemctl is-active -- " + service;
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

}  // namespace yukinal::ssh_service

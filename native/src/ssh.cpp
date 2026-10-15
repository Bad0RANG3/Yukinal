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
#include <cstring>
#include <deque>
#include <fstream>
#include <iostream>
#include <memory>
#include <mutex>
#include <string_view>
#include <thread>
#include <utility>
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
#include "host_key.hpp"

#include <libssh2.h>
#include <libssh2_sftp.h>
#include <nlohmann/json.hpp>
#include <openssl/ssl.h>

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

bool prevent_sigpipe(Socket socket) {
#ifdef SO_NOSIGPIPE
  int enabled = 1;
  return setsockopt(socket, SOL_SOCKET, SO_NOSIGPIPE,
                    reinterpret_cast<const char*>(&enabled), sizeof(enabled)) == 0;
#else
  (void)socket;
  return true;
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
  proxy_auth_required,
  proxy_connect_failed,
  proxy_tls_failed,
};

using SslPtr = std::unique_ptr<SSL, decltype(&SSL_free)>;

struct SocketConnection {
  Socket socket = kInvalidSocket;
  ConnectFailure failure = ConnectFailure::connection_failed;
  SslPtr proxy_tls{nullptr, SSL_free};
};

struct TerminalAuthContext;

struct Libssh2SessionContext {
  SSL* proxy_tls = nullptr;
  Socket socket = kInvalidSocket;
  Clock::time_point deadline = Clock::time_point::max();
  TerminalAuthContext* terminal_auth = nullptr;
};

std::string read_environment(const std::string& name);
void wipe(std::string* value);
std::string base64_encode(const std::vector<unsigned char>& bytes);

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

enum class ProxyTunnelFailure { none, timeout, authentication, resolver_busy, resolver_unavailable, rejected };

bool wait_socket_ready(Socket socket, bool writing, Clock::time_point deadline) {
  const int time_left = remaining_ms(deadline);
  if (time_left <= 0) return false;
  fd_set read_set;
  fd_set write_set;
  FD_ZERO(&read_set);
  FD_ZERO(&write_set);
  if (writing) FD_SET(socket, &write_set);
  else FD_SET(socket, &read_set);
  timeval timeout{};
  timeout.tv_sec = time_left / 1000;
  timeout.tv_usec = (time_left % 1000) * 1000;
#ifdef _WIN32
  return select(0, &read_set, &write_set, nullptr, &timeout) > 0;
#else
  return select(socket + 1, &read_set, &write_set, nullptr, &timeout) > 0;
#endif
}

bool wait_for_tls_retry(SSL* tls, Socket socket, int result,
                       Clock::time_point deadline) {
  const int error = SSL_get_error(tls, result);
  if (error == SSL_ERROR_WANT_READ) {
    return wait_socket_ready(socket, false, deadline);
  }
  if (error == SSL_ERROR_WANT_WRITE) {
    return wait_socket_ready(socket, true, deadline);
  }
  return false;
}

bool tls_write_all(SSL* tls, Socket socket, const void* data, std::size_t length,
                   Clock::time_point deadline) {
  const auto* bytes = static_cast<const unsigned char*>(data);
  std::size_t offset = 0;
  while (offset < length) {
    std::size_t written = 0;
    const int result = SSL_write_ex(tls, bytes + offset, length - offset, &written);
    if (result == 1 && written > 0) {
      offset += written;
      continue;
    }
    if (result != 1 && wait_for_tls_retry(tls, socket, result, deadline)) continue;
    return false;
  }
  return true;
}

bool tls_read_one(SSL* tls, Socket socket, char* byte, Clock::time_point deadline) {
  while (remaining_ms(deadline) > 0) {
    std::size_t received = 0;
    const int result = SSL_read_ex(tls, byte, 1, &received);
    if (result == 1) return received == 1;
    if (result != 1 && wait_for_tls_retry(tls, socket, result, deadline)) continue;
    return false;
  }
  return false;
}

SslPtr establish_verified_proxy_tls(const TargetConfig& target, Socket socket,
                                    Clock::time_point deadline) {
  using SslContextPtr = std::unique_ptr<SSL_CTX, decltype(&SSL_CTX_free)>;
  SslContextPtr context(SSL_CTX_new(TLS_client_method()), SSL_CTX_free);
  if (!context || SSL_CTX_set_default_verify_paths(context.get()) != 1) {
    return SslPtr(nullptr, SSL_free);
  }
  SSL_CTX_set_verify(context.get(), SSL_VERIFY_PEER, nullptr);
  if (SSL_CTX_set_min_proto_version(context.get(), TLS1_2_VERSION) != 1) {
    return SslPtr(nullptr, SSL_free);
  }

  SslPtr tls(SSL_new(context.get()), SSL_free);
  if (!tls) return tls;
  std::string verify_host = target.proxy_host;
  if (verify_host.size() >= 2 && verify_host.front() == '[' && verify_host.back() == ']') {
    verify_host = verify_host.substr(1, verify_host.size() - 2);
  }
  const bool ip_literal = verify_host.find(':') != std::string::npos ||
      (!verify_host.empty() && verify_host.find('.') != std::string::npos &&
       std::all_of(verify_host.begin(), verify_host.end(), [](unsigned char ch) {
         return (ch >= '0' && ch <= '9') || ch == '.';
       }));
  X509_VERIFY_PARAM* verify_parameters = SSL_get0_param(tls.get());
  if (ip_literal) {
    if (X509_VERIFY_PARAM_set1_ip_asc(verify_parameters, verify_host.c_str()) != 1) {
      return SslPtr(nullptr, SSL_free);
    }
  } else if (SSL_set1_host(tls.get(), verify_host.c_str()) != 1 ||
             SSL_set_tlsext_host_name(tls.get(), verify_host.c_str()) != 1) {
    return SslPtr(nullptr, SSL_free);
  }
  if (SSL_set_fd(tls.get(), static_cast<int>(socket)) != 1) {
    return SslPtr(nullptr, SSL_free);
  }
  SSL_set_connect_state(tls.get());
  SSL_set_mode(tls.get(), SSL_MODE_ENABLE_PARTIAL_WRITE |
                          SSL_MODE_ACCEPT_MOVING_WRITE_BUFFER);
  while (remaining_ms(deadline) > 0) {
    const int result = SSL_connect(tls.get());
    if (result == 1) {
      if (SSL_get_verify_result(tls.get()) != X509_V_OK) {
        return SslPtr(nullptr, SSL_free);
      }
      return tls;
    }
    if (!wait_for_tls_retry(tls.get(), socket, result, deadline)) break;
  }
  return SslPtr(nullptr, SSL_free);
}

ProxyTunnelFailure establish_http_connect_tunnel(const TargetConfig& target, Socket socket,
                                                  Clock::time_point deadline, SSL* tls) {
  std::string authority = target.host;
  if (authority.find(':') != std::string::npos && authority.front() != '[') {
    authority = "[" + authority + "]";
  }
  authority += ":" + std::to_string(target.port);

  std::string request = "CONNECT " + authority + " HTTP/1.1\r\nHost: " + authority +
                        "\r\nProxy-Connection: Keep-Alive\r\n";
  std::string encoded_credential;
  if (!target.proxy_auth_env.empty()) {
    std::string credential = read_environment(target.proxy_auth_env);
    if (credential.empty() || credential.size() > 1024) {
      wipe(&credential);
      return ProxyTunnelFailure::authentication;
    }
    std::vector<unsigned char> credential_bytes(credential.begin(), credential.end());
    encoded_credential = base64_encode(credential_bytes);
    volatile unsigned char* credential_data =
        credential_bytes.empty() ? nullptr : credential_bytes.data();
    for (std::size_t i = 0; i < credential_bytes.size(); ++i) credential_data[i] = 0;
    credential_bytes.clear();
    wipe(&credential);
    constexpr std::string_view authorization_prefix = "Proxy-Authorization: Basic ";
    request.reserve(request.size() + authorization_prefix.size() + encoded_credential.size() + 4);
    request.append(authorization_prefix);
    request.append(encoded_credential);
    request.append("\r\n");
  }
  request += "\r\n";
  wipe(&encoded_credential);

  std::size_t sent_total = 0;
  if (tls) {
    if (!tls_write_all(tls, socket, request.data(), request.size(), deadline)) {
      wipe(&request);
      return remaining_ms(deadline) == 0 ? ProxyTunnelFailure::timeout
                                         : ProxyTunnelFailure::rejected;
    }
    sent_total = request.size();
  }
  while (sent_total < request.size()) {
    if (!wait_socket_ready(socket, true, deadline)) {
      wipe(&request);
      return remaining_ms(deadline) == 0 ? ProxyTunnelFailure::timeout
                                         : ProxyTunnelFailure::rejected;
    }
    const auto remaining = request.size() - sent_total;
    const int chunk = static_cast<int>(std::min<std::size_t>(remaining, INT_MAX));
#ifdef MSG_NOSIGNAL
    constexpr int send_flags = MSG_NOSIGNAL;
#else
    constexpr int send_flags = 0;
#endif
    const int sent = send(socket, request.data() + sent_total, chunk, send_flags);
    if (sent <= 0) {
      wipe(&request);
      return remaining_ms(deadline) == 0 ? ProxyTunnelFailure::timeout
                                         : ProxyTunnelFailure::rejected;
    }
    sent_total += static_cast<std::size_t>(sent);
  }
  wipe(&request);

  std::string response;
  response.reserve(512);
  while (response.size() < 8192 && response.find("\r\n\r\n") == std::string::npos) {
    if (!tls && !wait_socket_ready(socket, false, deadline)) {
      return remaining_ms(deadline) == 0 ? ProxyTunnelFailure::timeout
                                         : ProxyTunnelFailure::rejected;
    }
    char byte = 0;
    const bool received = tls
        ? tls_read_one(tls, socket, &byte, deadline)
        : recv(socket, &byte, 1, 0) == 1;
    if (!received) {
      return remaining_ms(deadline) == 0 ? ProxyTunnelFailure::timeout
                                         : ProxyTunnelFailure::rejected;
    }
    response.push_back(byte);
  }
  const auto line_end = response.find("\r\n");
  if (line_end == std::string::npos || response.find("\r\n\r\n") == std::string::npos) {
    return ProxyTunnelFailure::rejected;
  }
  const std::string status_line = response.substr(0, line_end);
  if ((status_line.rfind("HTTP/1.0 ", 0) != 0 && status_line.rfind("HTTP/1.1 ", 0) != 0) ||
      status_line.size() < 12 || status_line[9] < '0' || status_line[9] > '9' ||
      status_line[10] < '0' || status_line[10] > '9' || status_line[11] < '0' ||
      status_line[11] > '9' || (status_line.size() > 12 && status_line[12] != ' ')) {
    return ProxyTunnelFailure::rejected;
  }
  const std::string status = status_line.substr(9, 3);
  if (status == "200") return ProxyTunnelFailure::none;
  if (status == "407") return ProxyTunnelFailure::authentication;
  return ProxyTunnelFailure::rejected;
}

bool proxy_socket_write_all(Socket socket, std::string_view bytes, Clock::time_point deadline) {
  std::size_t offset = 0;
  while (offset < bytes.size()) {
    if (!wait_socket_ready(socket, true, deadline)) return false;
    const auto remaining = bytes.size() - offset;
    const int chunk = static_cast<int>(std::min<std::size_t>(remaining, INT_MAX));
#ifdef MSG_NOSIGNAL
    constexpr int send_flags = MSG_NOSIGNAL;
#else
    constexpr int send_flags = 0;
#endif
    const int sent = send(socket, bytes.data() + offset, chunk, send_flags);
    if (sent > 0) {
      offset += static_cast<std::size_t>(sent);
    } else if (socket_would_block()) {
      continue;
    } else {
      return false;
    }
  }
  return true;
}

bool proxy_socket_read_exact(Socket socket, char* buffer, std::size_t length,
                             Clock::time_point deadline) {
  std::size_t offset = 0;
  while (offset < length) {
    if (!wait_socket_ready(socket, false, deadline)) return false;
    const int chunk = static_cast<int>(std::min<std::size_t>(length - offset, INT_MAX));
    const int received = recv(socket, buffer + offset, chunk, 0);
    if (received > 0) {
      offset += static_cast<std::size_t>(received);
    } else if (received < 0 && socket_would_block()) {
      continue;
    } else {
      return false;
    }
  }
  return true;
}

ProxyTunnelFailure append_socks_destination(const TargetConfig& target,
                                             Clock::time_point deadline,
                                             std::string* request) {
  if (!request) return ProxyTunnelFailure::rejected;
  std::string host = target.host;
  if (host.size() >= 2 && host.front() == '[' && host.back() == ']') {
    host = host.substr(1, host.size() - 2);
  }
  std::array<unsigned char, 16> address{};
  if (inet_pton(AF_INET, host.c_str(), address.data()) == 1) {
    request->push_back('\x01');
    request->append(reinterpret_cast<const char*>(address.data()), 4);
  } else if (inet_pton(AF_INET6, host.c_str(), address.data()) == 1) {
    request->push_back('\x04');
    request->append(reinterpret_cast<const char*>(address.data()), 16);
  } else if (target.proxy_protocol == ProxyProtocol::socks5_remote_dns) {
    if (host.empty() || host.size() > 255 || host.find(':') != std::string::npos) {
      return ProxyTunnelFailure::rejected;
    }
    request->push_back('\x03');
    request->push_back(static_cast<char>(host.size()));
    request->append(host);
  } else {
    addrinfo hints{};
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    addrinfo* raw_addresses = nullptr;
    const auto resolution = detail::resolve_address_list(
        host, std::to_string(target.port), hints, deadline, &raw_addresses);
    std::unique_ptr<addrinfo, decltype(&freeaddrinfo)> addresses(raw_addresses, freeaddrinfo);
    if (resolution == detail::AddressResolution::timed_out) return ProxyTunnelFailure::timeout;
    if (resolution == detail::AddressResolution::busy) return ProxyTunnelFailure::resolver_busy;
    if (resolution == detail::AddressResolution::unavailable) return ProxyTunnelFailure::resolver_unavailable;
    if (resolution != detail::AddressResolution::resolved || !addresses) {
      return ProxyTunnelFailure::rejected;
    }
    const addrinfo* selected = addresses.get();
    while (selected && selected->ai_family != AF_INET && selected->ai_family != AF_INET6) {
      selected = selected->ai_next;
    }
    if (!selected) return ProxyTunnelFailure::rejected;
    if (selected->ai_family == AF_INET) {
      const auto* ipv4 = reinterpret_cast<const sockaddr_in*>(selected->ai_addr);
      request->push_back('\x01');
      request->append(reinterpret_cast<const char*>(&ipv4->sin_addr), sizeof(ipv4->sin_addr));
    } else {
      const auto* ipv6 = reinterpret_cast<const sockaddr_in6*>(selected->ai_addr);
      request->push_back('\x04');
      request->append(reinterpret_cast<const char*>(&ipv6->sin6_addr), sizeof(ipv6->sin6_addr));
    }
  }
  request->push_back(static_cast<char>((target.port >> 8) & 0xff));
  request->push_back(static_cast<char>(target.port & 0xff));
  return ProxyTunnelFailure::none;
}

ProxyTunnelFailure establish_socks5_tunnel(const TargetConfig& target, Socket socket,
                                            Clock::time_point deadline) {
  const bool auth_required = !target.proxy_auth_env.empty();
  const std::string greeting = auth_required ? std::string("\x05\x01\x02", 3)
                                             : std::string("\x05\x01\x00", 3);
  if (!proxy_socket_write_all(socket, greeting, deadline)) {
    return remaining_ms(deadline) == 0 ? ProxyTunnelFailure::timeout : ProxyTunnelFailure::rejected;
  }
  std::array<char, 2> method_response{};
  if (!proxy_socket_read_exact(socket, method_response.data(), method_response.size(), deadline)) {
    return remaining_ms(deadline) == 0 ? ProxyTunnelFailure::timeout : ProxyTunnelFailure::rejected;
  }
  const auto version = static_cast<unsigned char>(method_response[0]);
  const auto method = static_cast<unsigned char>(method_response[1]);
  if (version != 5 || method == 0xff) return ProxyTunnelFailure::authentication;
  if (auth_required) {
    if (method != 2) return ProxyTunnelFailure::authentication;
    std::string credential = read_environment(target.proxy_auth_env);
    const auto separator = credential.find(':');
    if (credential.empty() || credential.size() > 1024 || separator == std::string::npos ||
        separator == 0 || separator + 1 >= credential.size() || separator > 255 ||
        credential.size() - separator - 1 > 255) {
      wipe(&credential);
      return ProxyTunnelFailure::authentication;
    }
    std::string auth_request;
    auth_request.reserve(3 + credential.size());
    auth_request.push_back('\x01');
    auth_request.push_back(static_cast<char>(separator));
    auth_request.append(credential, 0, separator);
    auth_request.push_back(static_cast<char>(credential.size() - separator - 1));
    auth_request.append(credential, separator + 1, std::string::npos);
    wipe(&credential);
    const bool sent = proxy_socket_write_all(socket, auth_request, deadline);
    wipe(&auth_request);
    if (!sent) {
      return remaining_ms(deadline) == 0 ? ProxyTunnelFailure::timeout : ProxyTunnelFailure::rejected;
    }
    std::array<char, 2> auth_response{};
    if (!proxy_socket_read_exact(socket, auth_response.data(), auth_response.size(), deadline)) {
      return remaining_ms(deadline) == 0 ? ProxyTunnelFailure::timeout : ProxyTunnelFailure::rejected;
    }
    if (static_cast<unsigned char>(auth_response[0]) != 1 ||
        static_cast<unsigned char>(auth_response[1]) != 0) {
      return ProxyTunnelFailure::authentication;
    }
  } else if (method != 0) {
    return ProxyTunnelFailure::authentication;
  }

  std::string request("\x05\x01\x00", 3);
  const auto destination = append_socks_destination(target, deadline, &request);
  if (destination != ProxyTunnelFailure::none) return destination;
  if (!proxy_socket_write_all(socket, request, deadline)) {
    return remaining_ms(deadline) == 0 ? ProxyTunnelFailure::timeout : ProxyTunnelFailure::rejected;
  }

  std::array<unsigned char, 4> response{};
  if (!proxy_socket_read_exact(socket, reinterpret_cast<char*>(response.data()), response.size(), deadline)) {
    return remaining_ms(deadline) == 0 ? ProxyTunnelFailure::timeout : ProxyTunnelFailure::rejected;
  }
  if (response[0] != 5 || response[2] != 0) return ProxyTunnelFailure::rejected;
  if (response[1] != 0) return ProxyTunnelFailure::rejected;
  std::size_t bound_address_size = 0;
  if (response[3] == 1) bound_address_size = 4;
  else if (response[3] == 4) bound_address_size = 16;
  else if (response[3] == 3) {
    char length = 0;
    if (!proxy_socket_read_exact(socket, &length, 1, deadline)) {
      return remaining_ms(deadline) == 0 ? ProxyTunnelFailure::timeout : ProxyTunnelFailure::rejected;
    }
    bound_address_size = static_cast<unsigned char>(length);
  } else {
    return ProxyTunnelFailure::rejected;
  }
  std::array<char, 257> bound_address{};
  if (bound_address_size > bound_address.size() ||
      !proxy_socket_read_exact(socket, bound_address.data(), bound_address_size + 2, deadline)) {
    return remaining_ms(deadline) == 0 ? ProxyTunnelFailure::timeout : ProxyTunnelFailure::rejected;
  }
  return ProxyTunnelFailure::none;
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
  const bool use_proxy = !target.proxy_host.empty() && !proxy_bypasses_target(target);
  const bool use_tls_proxy = use_proxy && target.proxy_protocol == ProxyProtocol::https_connect;
  const bool use_socks_proxy = use_proxy &&
      (target.proxy_protocol == ProxyProtocol::socks5_local_dns ||
       target.proxy_protocol == ProxyProtocol::socks5_remote_dns);
  const std::string& connect_host = use_proxy ? target.proxy_host : target.host;
  const int connect_port = use_proxy ? target.proxy_port : target.port;
  const std::string port_text = std::to_string(connect_port);
  const auto resolution = detail::resolve_address_list(connect_host, port_text, hints, deadline,
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
      if ((use_tls_proxy || use_socks_proxy || set_nonblocking(candidate, false)) &&
          prevent_sigpipe(candidate)) {
        connected = candidate;
      }
      if (connected != kInvalidSocket) break;
    }
    close_socket(candidate);
  }
  freeaddrinfo(addresses);
  if (connected != kInvalidSocket) {
    if (!use_proxy) return {connected, ConnectFailure::none};
    SslPtr proxy_tls(nullptr, SSL_free);
    if (use_tls_proxy) {
      proxy_tls = establish_verified_proxy_tls(target, connected, deadline);
      if (!proxy_tls) {
        close_socket(connected);
        return {kInvalidSocket, ConnectFailure::proxy_tls_failed};
      }
    }
    const auto tunnel = use_socks_proxy
        ? establish_socks5_tunnel(target, connected, deadline)
        : establish_http_connect_tunnel(target, connected, deadline, proxy_tls.get());
    if (tunnel == ProxyTunnelFailure::none) {
      if (use_socks_proxy && !set_nonblocking(connected, false)) {
        close_socket(connected);
        return {kInvalidSocket, ConnectFailure::proxy_connect_failed};
      }
      SocketConnection result;
      result.socket = connected;
      result.failure = ConnectFailure::none;
      result.proxy_tls = std::move(proxy_tls);
      return result;
    }
    close_socket(connected);
    if (tunnel == ProxyTunnelFailure::timeout) return {kInvalidSocket, ConnectFailure::timeout};
    if (tunnel == ProxyTunnelFailure::authentication) {
      return {kInvalidSocket, ConnectFailure::proxy_auth_required};
    }
    if (tunnel == ProxyTunnelFailure::resolver_busy) {
      return {kInvalidSocket, ConnectFailure::resolver_busy};
    }
    if (tunnel == ProxyTunnelFailure::resolver_unavailable) {
      return {kInvalidSocket, ConnectFailure::resolver_unavailable};
    }
    return {kInvalidSocket, ConnectFailure::proxy_connect_failed};
  }
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

void set_session_timeout_ms(LIBSSH2_SESSION* session, int timeout_ms) {
  if (!session || timeout_ms <= 0) return;
  libssh2_session_set_timeout(session, timeout_ms);
  void** abstract = libssh2_session_abstract(session);
  if (abstract && *abstract) {
    auto* context = static_cast<Libssh2SessionContext*>(*abstract);
    context->deadline = Clock::now() + std::chrono::milliseconds(timeout_ms);
  }
}

bool set_session_timeout(LIBSSH2_SESSION* session, Clock::time_point deadline) {
  const int time_left = remaining_ms(deadline);
  if (time_left <= 0) return false;
  set_session_timeout_ms(session, time_left);
  return true;
}

std::string read_environment(const std::string& name) {
  if (name.empty()) return {};
  const char* value = std::getenv(name.c_str());
  return value ? value : "";
}

bool read_public_key_file(const std::string& path, std::string* public_key) {
  if (path.empty() || !public_key) return false;
  std::ifstream input(path, std::ios::binary);
  if (!input) return false;
  public_key->clear();
  public_key->reserve(4096);
  char character = 0;
  while (input.get(character) && character != '\n') {
    if (public_key->size() == 64 * 1024) {
      public_key->clear();
      return false;
    }
    public_key->push_back(character);
  }
  if (input.bad()) {
    public_key->clear();
    return false;
  }
  if (!public_key->empty() && public_key->back() == '\r') public_key->pop_back();
  if (public_key->empty() || public_key->size() > 64 * 1024 ||
      public_key->find('\0') != std::string::npos) {
    public_key->clear();
    return false;
  }
  const bool printable = std::all_of(public_key->begin(), public_key->end(),
                                      [](unsigned char character) {
                                        return character >= 0x20 && character <= 0x7e;
                                      });
  if (!printable) public_key->clear();
  return printable;
}

ssize_t proxy_tls_read(libssh2_socket_t, void* buffer, std::size_t length,
                       int, void** abstract) {
  auto* context = abstract ? static_cast<Libssh2SessionContext*>(*abstract) : nullptr;
  if (!context || !context->proxy_tls || length == 0) return -EIO;
  while (remaining_ms(context->deadline) > 0) {
    std::size_t received = 0;
    const int result = SSL_read_ex(context->proxy_tls, buffer, length, &received);
    if (result == 1) return static_cast<ssize_t>(received);
    const int error = SSL_get_error(context->proxy_tls, result);
    if (error == SSL_ERROR_ZERO_RETURN) return 0;
    if (error == SSL_ERROR_WANT_READ) return -EAGAIN;
    if (error == SSL_ERROR_WANT_WRITE &&
        wait_socket_ready(context->socket, true, context->deadline)) {
      continue;
    }
    return -EIO;
  }
  return -EAGAIN;
}

ssize_t proxy_tls_write(libssh2_socket_t, const void* buffer, std::size_t length,
                        int, void** abstract) {
  auto* context = abstract ? static_cast<Libssh2SessionContext*>(*abstract) : nullptr;
  if (!context || !context->proxy_tls || length == 0) return -EIO;
  while (remaining_ms(context->deadline) > 0) {
    std::size_t written = 0;
    const int result = SSL_write_ex(context->proxy_tls, buffer, length, &written);
    if (result == 1) return static_cast<ssize_t>(written);
    const int error = SSL_get_error(context->proxy_tls, result);
    if (error == SSL_ERROR_WANT_WRITE) return -EAGAIN;
    if (error == SSL_ERROR_WANT_READ &&
        wait_socket_ready(context->socket, false, context->deadline)) {
      continue;
    }
    return -EIO;
  }
  return -EAGAIN;
}

class SshResources {
 public:
  ~SshResources() {
    if (agent_) {
      libssh2_agent_disconnect(agent_);
      libssh2_agent_free(agent_);
    }
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
    proxy_tls_.reset();
    close_socket(socket_);
  }

  bool initialize_session() {
    session_ = libssh2_session_init_ex(nullptr, nullptr, nullptr, &session_context_);
    if (!session_) return false;
    if (proxy_tls_) {
      libssh2_session_callback_set2(
          session_, LIBSSH2_CALLBACK_SEND,
          reinterpret_cast<libssh2_cb_generic*>(proxy_tls_write));
      libssh2_session_callback_set2(
          session_, LIBSSH2_CALLBACK_RECV,
          reinterpret_cast<libssh2_cb_generic*>(proxy_tls_read));
    }
    return true;
  }

  Socket socket_ = kInvalidSocket;
  SslPtr proxy_tls_{nullptr, SSL_free};
  Libssh2SessionContext session_context_;
  LIBSSH2_SESSION* session_ = nullptr;
  LIBSSH2_KNOWNHOSTS* known_hosts_ = nullptr;
  LIBSSH2_AGENT* agent_ = nullptr;
  LIBSSH2_SFTP* sftp_ = nullptr;
  LIBSSH2_SFTP_HANDLE* sftp_handle_ = nullptr;
  LIBSSH2_CHANNEL* channel_ = nullptr;
};

int known_host_key_type(detail::HostKeyAlgorithm algorithm) {
  switch (algorithm) {
    case detail::HostKeyAlgorithm::rsa: return LIBSSH2_KNOWNHOST_KEY_SSHRSA;
    case detail::HostKeyAlgorithm::dss: return LIBSSH2_KNOWNHOST_KEY_SSHDSS;
    case detail::HostKeyAlgorithm::ecdsa_256: return LIBSSH2_KNOWNHOST_KEY_ECDSA_256;
    case detail::HostKeyAlgorithm::ecdsa_384: return LIBSSH2_KNOWNHOST_KEY_ECDSA_384;
    case detail::HostKeyAlgorithm::ecdsa_521: return LIBSSH2_KNOWNHOST_KEY_ECDSA_521;
    case detail::HostKeyAlgorithm::ed25519: return LIBSSH2_KNOWNHOST_KEY_ED25519;
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

struct StringWiper {
  std::string* value;
  ~StringWiper() { wipe(value); }
};

void wipe_json_strings(nlohmann::json* value) {
  if (value->is_string()) {
    wipe(&value->get_ref<std::string&>());
  } else if (value->is_array()) {
    for (auto& item : *value) wipe_json_strings(&item);
  } else if (value->is_object()) {
    for (auto& item : *value) wipe_json_strings(&item);
  }
}

struct JsonStringWiper {
  nlohmann::json* value;
  ~JsonStringWiper() { wipe_json_strings(value); }
};

struct DurationTracker {
  ExecResult& result;
  Clock::time_point started;
  ~DurationTracker() {
    result.duration_ms = std::chrono::duration_cast<std::chrono::milliseconds>(Clock::now() - started).count();
  }
};

bool append_output(std::string* output, const char* buffer, std::size_t length,
                   std::size_t* remaining_bytes) {
  if (!remaining_bytes) return false;
  const std::size_t keep = std::min(*remaining_bytes, length);
  output->append(buffer, keep);
  *remaining_bytes -= keep;
  return keep == length;
}

int initialize_libssh2() {
  static std::once_flag initialized;
  static int result = 0;
  std::call_once(initialized, [] { result = libssh2_init(0); });
  return result;
}

struct TerminalInputQueue;

struct TerminalAuthContext {
  std::shared_ptr<TerminalInputQueue> input;
  bool failed = false;
};

void terminal_keyboard_interactive_callback(
    const char* name, int name_length, const char* instruction, int instruction_length,
    int prompt_count, const LIBSSH2_USERAUTH_KBDINT_PROMPT* prompts,
    LIBSSH2_USERAUTH_KBDINT_RESPONSE* responses, void** abstract);

bool connect_authenticated(const TargetConfig& target, Clock::time_point deadline,
                           SshResources* resources, ExecResult* result,
                           TerminalAuthContext* terminal_auth = nullptr) {
  if (initialize_libssh2() != 0) {
    fail(result, "ssh_unavailable", "SSH library initialization failed");
    return false;
  }

  std::string revocation_list;
  if (!target.host_ca_public_key.empty()) {
    std::string revocation_error;
    auto loaded = detail::load_host_revocation_list(target, &revocation_error);
    if (!loaded) {
      const std::string message = "Could not load the host certificate revocation list: " + revocation_error;
      fail(result, "host_key_unverified", message.c_str());
      return false;
    }
    revocation_list = std::move(*loaded);
  }

  SocketConnection connection = connect_socket(target, deadline);
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
      case ConnectFailure::proxy_auth_required:
        fail(result, "proxy_auth_required", "The configured HTTP proxy credential is missing or was rejected");
        break;
      case ConnectFailure::proxy_connect_failed:
        fail(result, "proxy_connect_failed", "Could not establish a tunnel through the configured proxy");
        break;
      case ConnectFailure::proxy_tls_failed:
        fail(result, "proxy_tls_failed", "Could not establish a verified TLS connection to the HTTP proxy");
        break;
      default:
        fail(result, "connection_failed", "Could not connect to the configured SSH target");
        break;
    }
    return false;
  }

  resources->proxy_tls_ = std::move(connection.proxy_tls);
  resources->session_context_.proxy_tls = resources->proxy_tls_.get();
  resources->session_context_.socket = resources->socket_;
  resources->session_context_.deadline = deadline;
  if (!resources->initialize_session()) {
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
  const char* host_key = libssh2_session_hostkey(resources->session_, &host_key_length, nullptr);
  const auto material = host_key
      ? detail::normalize_host_key_blob(std::string_view(host_key, host_key_length))
      : std::nullopt;
  const int known_key_type = material ? known_host_key_type(material->algorithm) : 0;
  if (!material || (target.host_ca_public_key.empty() &&
                    target.host_key_fingerprint.empty() && known_key_type == 0)) {
    fail(result, "host_key_unverified", "The SSH server presented an unsupported host key");
    return false;
  }
  const std::string fingerprint = detail::fingerprint_host_key_blob(
      std::string_view(host_key, host_key_length));
  if (fingerprint.empty()) {
    fail(result, "host_key_unverified", "Could not fingerprint the SSH host key");
    return false;
  }
  if (!target.host_ca_public_key.empty()) {
    if (!material->certificate) {
      fail(result, "host_key_unverified",
           "A host CA is configured, but the server presented a plain host key");
      return false;
    }
    std::string certificate_error;
    if (!detail::verify_host_certificate_with_krl(std::string_view(host_key, host_key_length),
            target.host_ca_public_key, target.host_principals, revocation_list,
            target.host_ca_revocation_list_signers, &certificate_error)) {
      const std::string message = "The SSH host certificate could not be verified: " + certificate_error;
      fail(result, "host_key_unverified", message.c_str());
      return false;
    }
  } else if (!target.host_key_fingerprint.empty()) {
    if (fingerprint != target.host_key_fingerprint) {
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
        resources->known_hosts_, target.host.c_str(), target.port,
        material->public_key_blob.data(), material->public_key_blob.size(),
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
  if (target.agent_auth) {
    resources->agent_ = libssh2_agent_init(resources->session_);
    if (!resources->agent_) {
      fail(result, "ssh_unavailable", "Could not initialize the SSH agent client");
      return false;
    }
    if (libssh2_agent_connect(resources->agent_) != 0) {
      fail(result, "credential_unavailable", "Could not connect to the configured SSH agent");
      return false;
    }
    if (libssh2_agent_list_identities(resources->agent_) != 0) {
      fail(result, "credential_unavailable", "Could not list identities from the SSH agent");
      return false;
    }
    struct libssh2_agent_publickey* identity = nullptr;
    struct libssh2_agent_publickey* previous = nullptr;
    bool authenticated = false;
    for (;;) {
      if (!set_session_timeout(resources->session_, deadline)) {
        fail(result, "timeout", "SSH agent authentication timed out");
        return false;
      }
      const int identity_result =
          libssh2_agent_get_identity(resources->agent_, &identity, previous);
      if (identity_result == 1) break;
      if (identity_result < 0) {
        fail(result, "credential_unavailable", "Could not read an identity from the SSH agent");
        return false;
      }
      previous = identity;
      auth_result = libssh2_agent_userauth(resources->agent_, target.username.c_str(), identity);
      if (auth_result == 0) {
        authenticated = true;
        break;
      }
      if (remaining_ms(deadline) == 0 ||
          libssh2_session_last_errno(resources->session_) == LIBSSH2_ERROR_TIMEOUT) {
        fail(result, "timeout", "SSH agent authentication timed out");
        return false;
      }
    }
    if (!authenticated) {
      if (!terminal_auth) {
        fail(result, "authentication_failed", "None of the configured SSH agent identities was accepted");
        return false;
      }
    }
    if (authenticated) return true;
  }
  else if (!target.private_key_file.empty() || !target.private_key_env.empty()) {
    std::string passphrase = read_environment(target.passphrase_env);
    if (!target.private_key_env.empty()) {
      std::string private_key = read_environment(target.private_key_env);
      std::string public_key;
      if (private_key.empty()) {
        fail(result, "credential_unavailable", "The configured SSH private key is not available");
        wipe(&passphrase);
        return false;
      }
      if (!target.public_key_file.empty() &&
          !read_public_key_file(target.public_key_file, &public_key)) {
        fail(result, "credential_unavailable", "The configured SSH public key or certificate is not available");
        wipe(&private_key);
        wipe(&passphrase);
        return false;
      }
      auth_result = libssh2_userauth_publickey_frommemory(
          resources->session_, target.username.c_str(), target.username.size(),
          public_key.empty() ? nullptr : public_key.data(), public_key.size(),
          private_key.data(), private_key.size(), passphrase.empty() ? nullptr : passphrase.c_str());
      wipe(&public_key);
      wipe(&private_key);
    } else {
      auth_result = libssh2_userauth_publickey_fromfile(
          resources->session_, target.username.c_str(),
          target.public_key_file.empty() ? nullptr : target.public_key_file.c_str(),
          target.private_key_file.c_str(),
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
    bool timed_out = remaining_ms(deadline) == 0 ||
        libssh2_session_last_errno(resources->session_) == LIBSSH2_ERROR_TIMEOUT;
    if (!timed_out && terminal_auth) {
      const auto interactive_deadline = Clock::now() + std::chrono::seconds(125);
      if (set_session_timeout(resources->session_, interactive_deadline)) {
        resources->session_context_.terminal_auth = terminal_auth;
        auth_result = libssh2_userauth_keyboard_interactive_ex(
            resources->session_, target.username.c_str(),
            static_cast<unsigned int>(target.username.size()),
            terminal_keyboard_interactive_callback);
        resources->session_context_.terminal_auth = nullptr;
        timed_out = remaining_ms(interactive_deadline) == 0 ||
            libssh2_session_last_errno(resources->session_) == LIBSSH2_ERROR_TIMEOUT;
      }
    }
    if (auth_result == 0) return true;
    if (terminal_auth && terminal_auth->failed && !timed_out) {
      fail(result, "authentication_failed",
           "SSH keyboard-interactive authentication was cancelled or invalid");
      return false;
    }
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

namespace {

struct TerminalInputCommand {
  enum class Kind { write, resize, close, auth_response } kind = Kind::close;
  std::string data;
  std::vector<std::string> responses;
  std::uint16_t columns = 0;
  std::uint16_t rows = 0;
};

struct TerminalInputQueue {
  std::mutex mutex;
  std::condition_variable changed;
  std::deque<TerminalInputCommand> commands;
  std::size_t queued_data_bytes = 0;
  std::size_t expected_auth_responses = 0;
  bool auth_prompt_active = false;
  bool eof = false;
  bool invalid = false;
};

constexpr std::size_t kMaxTerminalInputFrameBytes = 256 * 1024;
constexpr std::size_t kMaxTerminalInputJsonBytes = kMaxTerminalInputFrameBytes * 6 + 256;
constexpr std::size_t kMaxQueuedTerminalInputBytes = 1024 * 1024;
constexpr std::size_t kMaxQueuedTerminalCommands = 256;

void read_terminal_input(const std::shared_ptr<TerminalInputQueue>& input) {
  std::string line;
  while (std::getline(std::cin, line)) {
    StringWiper line_wiper{&line};
    if (line.size() > kMaxTerminalInputJsonBytes) {
      std::lock_guard lock(input->mutex);
      input->invalid = true;
      break;
    }
    try {
      auto message = nlohmann::json::parse(line);
      wipe(&line);
      JsonStringWiper message_wiper{&message};
      if (!message.is_object() || !message.contains("type") || !message["type"].is_string()) {
        std::lock_guard lock(input->mutex);
        input->invalid = true;
        break;
      }
      const std::string type = message["type"].get<std::string>();
      TerminalInputCommand command;
      if (type == "write" && message.size() == 2 && message.contains("data") &&
          message["data"].is_string()) {
        command.kind = TerminalInputCommand::Kind::write;
        command.data = message["data"].get<std::string>();
        if (command.data.size() > kMaxTerminalInputFrameBytes) {
          std::lock_guard lock(input->mutex);
          input->invalid = true;
          break;
        }
      } else if (type == "resize" && message.size() == 3 && message.contains("columns") &&
                 message.contains("rows") && message["columns"].is_number_unsigned() &&
                 message["rows"].is_number_unsigned()) {
        const auto columns = message["columns"].get<unsigned int>();
        const auto rows = message["rows"].get<unsigned int>();
        if (columns == 0 || columns > 1000 || rows == 0 || rows > 1000) {
          std::lock_guard lock(input->mutex);
          input->invalid = true;
          break;
        }
        command.kind = TerminalInputCommand::Kind::resize;
        command.columns = static_cast<std::uint16_t>(columns);
        command.rows = static_cast<std::uint16_t>(rows);
      } else if (type == "auth_response" && message.size() == 2 &&
                 message.contains("responses") && message["responses"].is_array() &&
                 message["responses"].size() <= 16) {
        command.kind = TerminalInputCommand::Kind::auth_response;
        bool valid_responses = true;
        for (auto& response : message["responses"]) {
          if (!response.is_string()) {
            valid_responses = false;
            break;
          }
          auto& secret = response.get_ref<std::string&>();
          if (secret.size() > 4096) {
            valid_responses = false;
            break;
          }
          command.responses.push_back(std::move(secret));
        }
        std::lock_guard lock(input->mutex);
        if (!valid_responses || !input->auth_prompt_active ||
            command.responses.size() != input->expected_auth_responses) {
          for (auto& response : command.responses) wipe(&response);
          input->invalid = true;
          input->changed.notify_all();
          break;
        }
        input->auth_prompt_active = false;
        input->expected_auth_responses = 0;
        input->commands.push_back(std::move(command));
        input->changed.notify_all();
        continue;
      } else if (type == "close" && message.size() == 1) {
        command.kind = TerminalInputCommand::Kind::close;
        std::lock_guard lock(input->mutex);
        input->commands.push_back(std::move(command));
        input->changed.notify_all();
        break;
      } else {
        std::lock_guard lock(input->mutex);
        input->invalid = true;
        break;
      }

      std::unique_lock lock(input->mutex);
      input->changed.wait(lock, [&] {
        return input->eof || input->auth_prompt_active ||
               (input->commands.size() < kMaxQueuedTerminalCommands &&
                input->queued_data_bytes + command.data.size() <= kMaxQueuedTerminalInputBytes);
      });
      if (input->eof || input->auth_prompt_active) {
        input->invalid = true;
        input->changed.notify_all();
        break;
      }
      input->queued_data_bytes += command.data.size();
      input->commands.push_back(std::move(command));
      input->changed.notify_all();
    } catch (const nlohmann::json::exception&) {
      wipe(&line);
      std::lock_guard lock(input->mutex);
      input->invalid = true;
      break;
    }
  }
  {
    std::lock_guard lock(input->mutex);
    input->eof = true;
  }
  input->changed.notify_all();
}

bool emit_terminal_message(const nlohmann::json& message) {
  std::cout << message.dump(-1, ' ', false, nlohmann::json::error_handler_t::replace)
            << '\n' << std::flush;
  return std::cout.good();
}

void terminal_keyboard_interactive_callback(
    const char* name, int name_length, const char* instruction, int instruction_length,
    int prompt_count, const LIBSSH2_USERAUTH_KBDINT_PROMPT* prompts,
    LIBSSH2_USERAUTH_KBDINT_RESPONSE* responses, void** abstract) {
  auto* session_context = abstract
      ? static_cast<Libssh2SessionContext*>(*abstract)
      : nullptr;
  auto* context = session_context ? session_context->terminal_auth : nullptr;
  if (!context || !context->input || prompt_count < 0 || prompt_count > 16 ||
      (prompt_count > 0 && (!prompts || !responses)) || name_length < 0 ||
      instruction_length < 0 || name_length > 4096 || instruction_length > 4096 ||
      (name_length > 0 && !name) || (instruction_length > 0 && !instruction)) {
    if (context) context->failed = true;
    return;
  }

  nlohmann::json prompt_values = nlohmann::json::array();
  for (int index = 0; index < prompt_count; ++index) {
    if (prompts[index].length > 4096 || (prompts[index].length > 0 && !prompts[index].text)) {
      context->failed = true;
      return;
    }
    const std::string prompt = prompts[index].length == 0
        ? std::string{}
        : std::string(reinterpret_cast<const char*>(prompts[index].text), prompts[index].length);
    prompt_values.push_back({{"prompt", prompt}, {"echo", prompts[index].echo != 0}});
  }

  auto input = context->input;
  {
    std::lock_guard lock(input->mutex);
    if (input->eof || input->invalid || input->auth_prompt_active) {
      context->failed = true;
      return;
    }
    input->auth_prompt_active = true;
    input->expected_auth_responses = static_cast<std::size_t>(prompt_count);
  }
  input->changed.notify_all();
  if (!emit_terminal_message({
          {"type", "auth_prompt"},
          {"name", name_length == 0 ? std::string{} : std::string(name, static_cast<std::size_t>(name_length))},
          {"instructions", instruction_length == 0 ? std::string{} : std::string(instruction, static_cast<std::size_t>(instruction_length))},
          {"prompts", std::move(prompt_values)},
      })) {
    std::lock_guard lock(input->mutex);
    input->auth_prompt_active = false;
    input->expected_auth_responses = 0;
    context->failed = true;
    input->changed.notify_all();
    return;
  }

  TerminalInputCommand command;
  bool received_response = false;
  {
    std::unique_lock lock(input->mutex);
    const bool ready = input->changed.wait_for(lock, std::chrono::seconds(125), [&] {
      return input->invalid || input->eof || !input->commands.empty();
    });
    if (ready && !input->commands.empty() &&
        input->commands.front().kind == TerminalInputCommand::Kind::auth_response) {
      command = std::move(input->commands.front());
      input->commands.pop_front();
      received_response = command.responses.size() == static_cast<std::size_t>(prompt_count);
    }
    input->auth_prompt_active = false;
    input->expected_auth_responses = 0;
    if (!received_response) context->failed = true;
  }
  input->changed.notify_all();
  if (!received_response) return;

  std::vector<char*> allocated;
  allocated.reserve(command.responses.size());
  for (const auto& response : command.responses) {
    auto* value = static_cast<char*>(std::malloc(response.size() + 1));
    if (!value) {
      for (char* item : allocated) std::free(item);
      context->failed = true;
      for (auto& secret : command.responses) wipe(&secret);
      return;
    }
    std::memcpy(value, response.data(), response.size());
    value[response.size()] = '\0';
    allocated.push_back(value);
  }
  for (int index = 0; index < prompt_count; ++index) {
    const auto response_index = static_cast<std::size_t>(index);
    responses[index].text = allocated[response_index];
    responses[index].length = static_cast<unsigned int>(command.responses[response_index].size());
  }
  for (auto& secret : command.responses) wipe(&secret);
}

bool write_terminal_channel(SshResources* resources, const std::string& data) {
  std::size_t offset = 0;
  const auto deadline = Clock::now() + std::chrono::seconds(10);
  while (offset < data.size()) {
    const auto count = libssh2_channel_write_ex(
        resources->channel_, 0, data.data() + offset,
        static_cast<std::size_t>(data.size() - offset));
    if (count > 0) {
      offset += static_cast<std::size_t>(count);
      continue;
    }
    if (count == LIBSSH2_ERROR_EAGAIN &&
        wait_for_ssh(resources->socket_, resources->session_, deadline)) {
      continue;
    }
    return false;
  }
  return true;
}

}  // namespace

int run_terminal_session(const TargetConfig& target, std::uint16_t columns,
                         std::uint16_t rows) {
  if (columns == 0 || columns > 1000 || rows == 0 || rows > 1000 || target.probe_only) {
    emit_terminal_message({{"type", "error"}, {"message", "The terminal dimensions or SSH target are invalid"}});
    return 2;
  }
  const auto input = std::make_shared<TerminalInputQueue>();
  std::thread(read_terminal_input, input).detach();
  TerminalAuthContext terminal_auth{input};
  ExecResult connection_result;
  SshResources resources;
  const auto deadline = Clock::now() + std::chrono::seconds(30);
  if (!connect_authenticated(target, deadline, &resources, &connection_result, &terminal_auth)) {
    emit_terminal_message({{"type", "error"},
                           {"code", connection_result.error_code},
                           {"message", connection_result.error_message.empty()
                                           ? "SSH terminal connection failed"
                                           : connection_result.error_message}});
    return 2;
  }
  if (!set_session_timeout(resources.session_, deadline)) {
    emit_terminal_message({{"type", "error"}, {"code", "timeout"}, {"message", "SSH terminal startup timed out"}});
    return 2;
  }
  resources.channel_ = libssh2_channel_open_session(resources.session_);
  if (!resources.channel_) {
    emit_terminal_message({{"type", "error"}, {"code", "ssh_error"}, {"message", "Could not open an SSH terminal channel"}});
    return 2;
  }
  const std::string terminal_type = "xterm-256color";
  if (!set_session_timeout(resources.session_, deadline) ||
      libssh2_channel_request_pty_ex(resources.channel_, terminal_type.data(),
                                     static_cast<unsigned int>(terminal_type.size()), nullptr, 0,
                                     columns, rows, 0, 0) != 0 ||
      !set_session_timeout(resources.session_, deadline) ||
      libssh2_channel_process_startup(resources.channel_, "shell", 5, nullptr, 0) != 0) {
    emit_terminal_message({{"type", "error"}, {"code", "ssh_error"},
                           {"message", "Could not start the remote SSH terminal"}});
    return 2;
  }
  if (!set_nonblocking(resources.socket_, true)) {
    emit_terminal_message({{"type", "error"}, {"code", "ssh_error"}, {"message", "Could not configure the SSH terminal socket"}});
    return 2;
  }
  libssh2_session_set_blocking(resources.session_, 0);
  libssh2_channel_set_blocking(resources.channel_, 0);

  if (!emit_terminal_message({{"type", "ready"}})) return 0;

  bool running = true;
  bool failed = false;
  int exit_code = -1;
  std::array<char, 8192> buffer{};
  while (running) {
    for (;;) {
      TerminalInputCommand command;
      bool has_command = false;
      bool invalid_input = false;
      bool input_ended = false;
      {
        std::lock_guard lock(input->mutex);
        invalid_input = input->invalid;
        input_ended = input->eof && input->commands.empty();
        if (!input->commands.empty()) {
          command = std::move(input->commands.front());
          input->commands.pop_front();
          input->queued_data_bytes -= command.data.size();
          has_command = true;
        }
      }
      input->changed.notify_all();
      if (invalid_input) {
        emit_terminal_message({{"type", "error"}, {"message", "The SSH terminal input frame is invalid"}});
        failed = true;
        running = false;
        break;
      }
      if (input_ended) {
        running = false;
        break;
      }
      if (!has_command) break;
      if (command.kind == TerminalInputCommand::Kind::close) {
        running = false;
        break;
      }
      if (command.kind == TerminalInputCommand::Kind::write) {
        if (!write_terminal_channel(&resources, command.data)) {
          emit_terminal_message({{"type", "error"}, {"message", "Writing to the SSH terminal failed"}});
          failed = true;
          running = false;
          break;
        }
      } else {
        const auto resize_deadline = Clock::now() + std::chrono::seconds(5);
        int result = 0;
        do {
          result = libssh2_channel_request_pty_size(resources.channel_, command.columns,
                                                    command.rows);
          if (result == LIBSSH2_ERROR_EAGAIN &&
              wait_for_ssh(resources.socket_, resources.session_, resize_deadline)) {
            continue;
          }
          break;
        } while (true);
        if (result != 0) {
          emit_terminal_message({{"type", "error"}, {"message", "Resizing the SSH terminal failed"}});
          failed = true;
          running = false;
          break;
        }
      }
    }
    if (!running) break;

    bool made_progress = false;
    for (const int stream_id : {0, SSH_EXTENDED_DATA_STDERR}) {
      for (;;) {
        const auto count = libssh2_channel_read_ex(resources.channel_, stream_id,
                                                  buffer.data(), buffer.size());
        if (count == LIBSSH2_ERROR_EAGAIN || count == 0) break;
        if (count < 0) {
          emit_terminal_message({{"type", "error"}, {"message", "Reading the SSH terminal failed"}});
          failed = true;
          running = false;
          break;
        }
        made_progress = true;
        std::vector<unsigned char> bytes(buffer.begin(), buffer.begin() + count);
        if (!emit_terminal_message({{"type", "output"}, {"data", base64_encode(bytes)}})) {
          running = false;
          break;
        }
      }
      if (!running) break;
    }
    if (!running) break;
    if (libssh2_channel_eof(resources.channel_)) {
      exit_code = libssh2_channel_get_exit_status(resources.channel_);
      break;
    }
    if (!made_progress) {
      (void)wait_for_ssh(resources.socket_, resources.session_,
                         Clock::now() + std::chrono::milliseconds(40));
    }
  }

  if (resources.channel_ && !libssh2_channel_eof(resources.channel_)) {
    (void)libssh2_channel_send_eof(resources.channel_);
    (void)libssh2_channel_close(resources.channel_);
  }
  emit_terminal_message({{"type", "closed"},
                         {"exit_code", exit_code < 0 ? nlohmann::json(nullptr)
                                                       : nlohmann::json(exit_code)}});
  return failed ? 1 : 0;
}

namespace {

struct SftpStreamCommand {
  bool close = false;
  std::vector<unsigned char> data;
};

struct SftpStreamInput {
  std::mutex mutex;
  std::condition_variable changed;
  std::deque<SftpStreamCommand> commands;
  std::size_t queued_data_bytes = 0;
  bool eof = false;
  bool invalid = false;
};

constexpr std::size_t kSftpStreamChunkBytes = 32 * 1024;
constexpr std::size_t kSftpStreamFrameBytes = 48 * 1024;
constexpr std::size_t kSftpStreamMaxQueueBytes = 1024 * 1024;
constexpr std::size_t kSftpStreamMaxQueueCommands = 64;

int sftp_base64_value(char value) {
  if (value >= 'A' && value <= 'Z') return value - 'A';
  if (value >= 'a' && value <= 'z') return value - 'a' + 26;
  if (value >= '0' && value <= '9') return value - '0' + 52;
  if (value == '+') return 62;
  if (value == '/') return 63;
  return -1;
}

bool decode_sftp_stream_data(std::string_view encoded,
                             std::vector<unsigned char>* decoded) {
  if (!decoded || encoded.empty() || encoded.size() % 4 != 0 ||
      encoded.size() > ((kSftpStreamChunkBytes + 2) / 3) * 4) {
    return false;
  }
  const std::size_t padding = encoded.ends_with("==") ? 2 : encoded.ends_with("=") ? 1 : 0;
  for (std::size_t index = 0; index < encoded.size(); ++index) {
    const bool is_padding = encoded[index] == '=';
    if ((is_padding && (padding == 0 || index < encoded.size() - padding)) ||
        (!is_padding && sftp_base64_value(encoded[index]) < 0)) {
      return false;
    }
  }
  if (padding == 2 &&
      (sftp_base64_value(encoded[encoded.size() - 3]) & 0x0f) != 0) return false;
  if (padding == 1 &&
      (sftp_base64_value(encoded[encoded.size() - 2]) & 0x03) != 0) return false;

  decoded->clear();
  decoded->reserve(encoded.size() / 4 * 3 - padding);
  for (std::size_t index = 0; index < encoded.size(); index += 4) {
    const unsigned int first = static_cast<unsigned int>(sftp_base64_value(encoded[index]));
    const unsigned int second = static_cast<unsigned int>(sftp_base64_value(encoded[index + 1]));
    const unsigned int third = encoded[index + 2] == '=' ? 0U :
        static_cast<unsigned int>(sftp_base64_value(encoded[index + 2]));
    const unsigned int fourth = encoded[index + 3] == '=' ? 0U :
        static_cast<unsigned int>(sftp_base64_value(encoded[index + 3]));
    const unsigned int block = (first << 18) | (second << 12) | (third << 6) | fourth;
    decoded->push_back(static_cast<unsigned char>((block >> 16) & 0xff));
    if (encoded[index + 2] != '=') decoded->push_back(static_cast<unsigned char>((block >> 8) & 0xff));
    if (encoded[index + 3] != '=') decoded->push_back(static_cast<unsigned char>(block & 0xff));
  }
  return decoded->size() <= kSftpStreamChunkBytes;
}

void read_sftp_stream_input(const std::shared_ptr<SftpStreamInput>& input) {
  std::string line;
  while (std::getline(std::cin, line)) {
    if (line.size() > kSftpStreamFrameBytes) {
      std::lock_guard lock(input->mutex);
      input->invalid = true;
      break;
    }
    try {
      const auto message = nlohmann::json::parse(line);
      if (!message.is_object() || !message.contains("type") || !message["type"].is_string()) {
        std::lock_guard lock(input->mutex);
        input->invalid = true;
        break;
      }
      const std::string type = message["type"].get<std::string>();
      SftpStreamCommand command;
      if (type == "data" && message.size() == 2 && message.contains("data") &&
          message["data"].is_string() && decode_sftp_stream_data(
              message["data"].get_ref<const std::string&>(), &command.data)) {
        // The decoded payload is bounded to one 32 KiB transfer chunk.
      } else if (type == "close" && message.size() == 1) {
        command.close = true;
        std::lock_guard lock(input->mutex);
        input->commands.push_back(std::move(command));
        input->changed.notify_all();
        break;
      } else {
        std::lock_guard lock(input->mutex);
        input->invalid = true;
        break;
      }

      std::unique_lock lock(input->mutex);
      input->changed.wait(lock, [&] {
        return input->eof ||
               (input->commands.size() < kSftpStreamMaxQueueCommands &&
                input->queued_data_bytes + command.data.size() <= kSftpStreamMaxQueueBytes);
      });
      if (input->eof) break;
      input->queued_data_bytes += command.data.size();
      input->commands.push_back(std::move(command));
      input->changed.notify_all();
    } catch (const nlohmann::json::exception&) {
      std::lock_guard lock(input->mutex);
      input->invalid = true;
      break;
    }
  }
  {
    std::lock_guard lock(input->mutex);
    input->eof = true;
  }
  input->changed.notify_all();
}

bool write_sftp_stream_chunk(SshResources* resources,
                             const std::vector<unsigned char>& data) {
  std::size_t offset = 0;
  auto deadline = Clock::now() + std::chrono::seconds(30);
  while (offset < data.size()) {
    const auto count = libssh2_sftp_write(
        resources->sftp_handle_, reinterpret_cast<const char*>(data.data() + offset),
        data.size() - offset);
    if (count > 0) {
      offset += static_cast<std::size_t>(count);
      deadline = Clock::now() + std::chrono::seconds(30);
      continue;
    }
    if (count == LIBSSH2_ERROR_EAGAIN &&
        wait_for_ssh(resources->socket_, resources->session_, deadline)) continue;
    return false;
  }
  return true;
}

bool sync_sftp_stream_handle(SshResources* resources, Clock::time_point deadline) {
  for (;;) {
    const int result = libssh2_sftp_fsync(resources->sftp_handle_);
    if (result == 0) return true;
    if (result != LIBSSH2_ERROR_EAGAIN ||
        !wait_for_ssh(resources->socket_, resources->session_, deadline)) return false;
  }
}

bool close_sftp_stream_handle(SshResources* resources, Clock::time_point deadline) {
  if (!resources->sftp_handle_) return true;
  for (;;) {
    const int result = libssh2_sftp_close_handle(resources->sftp_handle_);
    if (result == LIBSSH2_ERROR_EAGAIN) {
      if (wait_for_ssh(resources->socket_, resources->session_, deadline)) continue;
      return false;
    }
    resources->sftp_handle_ = nullptr;
    return result == 0;
  }
}

void remove_failed_sftp_stream(SshResources* resources, const std::string& path) {
  const auto deadline = Clock::now() + std::chrono::seconds(5);
  for (;;) {
    const int result = libssh2_sftp_unlink_ex(
        resources->sftp_, path.data(), static_cast<unsigned int>(path.size()));
    if (result != LIBSSH2_ERROR_EAGAIN ||
        !wait_for_ssh(resources->socket_, resources->session_, deadline)) return;
  }
}

bool emit_sftp_stream_error(const std::string& code, const std::string& message) {
  return emit_terminal_message({{"type", "error"}, {"code", code}, {"message", message}});
}

}  // namespace

int run_sftp_stream(const TargetConfig& target, const std::string& path, bool upload) {
  if (path.empty() || path.size() > 4096 || path.find('\0') != std::string::npos) {
    emit_sftp_stream_error("invalid_parameters", "The SFTP stream path is invalid");
    return 2;
  }

  ExecResult connection_result;
  SshResources resources;
  const auto startup_deadline = Clock::now() + std::chrono::seconds(30);
  if (!connect_sftp(target, startup_deadline, &resources, &connection_result)) {
    emit_sftp_stream_error(connection_result.error_code.empty() ? "sftp_error" : connection_result.error_code,
                           connection_result.error_message.empty() ? "SFTP connection failed" : connection_result.error_message);
    return 2;
  }
  const long flags = upload
      ? LIBSSH2_FXF_WRITE | LIBSSH2_FXF_CREAT | LIBSSH2_FXF_EXCL
      : LIBSSH2_FXF_READ;
  if (!set_session_timeout(resources.session_, startup_deadline)) {
    emit_sftp_stream_error("timeout", "SFTP stream startup timed out");
    return 2;
  }
  resources.sftp_handle_ = libssh2_sftp_open_ex(
      resources.sftp_, path.data(), static_cast<unsigned int>(path.size()), flags,
      upload ? 0644 : 0, LIBSSH2_SFTP_OPENFILE);
  if (!resources.sftp_handle_) {
    const bool timed_out = remaining_ms(startup_deadline) == 0;
    emit_sftp_stream_error(timed_out ? "timeout" : "sftp_error",
                           upload ? "Could not exclusively create the remote staging file"
                                   : "Could not open the remote file for reading");
    return 2;
  }
  if (!set_nonblocking(resources.socket_, true)) {
    emit_sftp_stream_error("sftp_error", "Could not configure the SFTP stream socket");
    return 2;
  }
  libssh2_session_set_blocking(resources.session_, 0);
  if (!emit_terminal_message({{"type", "ready"}})) return 0;

  bool succeeded = true;
  std::uint64_t transferred = 0;
  std::array<char, kSftpStreamChunkBytes> buffer{};
  if (!upload) {
    auto deadline = Clock::now() + std::chrono::seconds(30);
    for (;;) {
      const auto count = libssh2_sftp_read(resources.sftp_handle_, buffer.data(), buffer.size());
      if (count == 0) break;
      if (count == LIBSSH2_ERROR_EAGAIN) {
        if (!wait_for_ssh(resources.socket_, resources.session_, deadline)) {
          emit_sftp_stream_error("timeout", "SFTP download timed out");
          succeeded = false;
          break;
        }
        continue;
      }
      if (count < 0) {
        emit_sftp_stream_error("sftp_error", "Reading the remote file failed");
        succeeded = false;
        break;
      }
      deadline = Clock::now() + std::chrono::seconds(30);
      transferred += static_cast<std::uint64_t>(count);
      std::vector<unsigned char> bytes(buffer.begin(), buffer.begin() + count);
      if (!emit_terminal_message({{"type", "data"}, {"data", base64_encode(bytes)}})) {
        succeeded = false;
        break;
      }
    }
  } else {
    const auto input = std::make_shared<SftpStreamInput>();
    std::thread(read_sftp_stream_input, input).detach();
    bool close_received = false;
    auto input_deadline = Clock::now() + std::chrono::seconds(60);
    while (succeeded && !close_received) {
      SftpStreamCommand command;
      bool has_command = false;
      bool invalid_input = false;
      bool input_ended = false;
      bool input_timed_out = false;
      {
        std::unique_lock lock(input->mutex);
        const bool ready = input->changed.wait_until(lock, input_deadline, [&] {
          return input->invalid || input->eof || !input->commands.empty();
        });
        input_timed_out = !ready;
        invalid_input = input->invalid;
        input_ended = input->eof && input->commands.empty();
        if (!input->commands.empty()) {
          command = std::move(input->commands.front());
          input->commands.pop_front();
          input->queued_data_bytes -= command.data.size();
          has_command = true;
        }
      }
      input->changed.notify_all();
      if (input_timed_out) {
        emit_sftp_stream_error("timeout", "The SFTP upload input was idle for 60 seconds");
        succeeded = false;
      } else if (invalid_input) {
        emit_sftp_stream_error("invalid_input", "The SFTP upload frame is invalid");
        succeeded = false;
      } else if (input_ended && !has_command) {
        emit_sftp_stream_error("cancelled", "The SFTP upload ended before its close frame");
        succeeded = false;
      } else if (has_command && command.close) {
        close_received = true;
      } else if (has_command) {
        if (!write_sftp_stream_chunk(&resources, command.data)) {
          emit_sftp_stream_error("sftp_error", "Writing the remote staging file failed");
          succeeded = false;
        } else {
          transferred += command.data.size();
          input_deadline = Clock::now() + std::chrono::seconds(60);
        }
      }
    }
    if (succeeded && !close_received) succeeded = false;
    if (succeeded && !sync_sftp_stream_handle(
            &resources, Clock::now() + std::chrono::seconds(30))) {
      emit_sftp_stream_error("sftp_error", "The SFTP server could not sync the staging file");
      succeeded = false;
    }
  }

  if (resources.sftp_handle_ && !close_sftp_stream_handle(
          &resources, Clock::now() + std::chrono::seconds(30)) && succeeded) {
    emit_sftp_stream_error("sftp_error", "Closing the SFTP stream failed");
    succeeded = false;
  }
  if (upload && !succeeded) {
    remove_failed_sftp_stream(&resources, path);
  }
  if (succeeded) {
    emit_terminal_message({{"type", "closed"}, {"exit_code", 0}, {"bytes", transferred}});
    return 0;
  }
  return 1;
}

static ExecResult run_ssh_command_with_limits(const TargetConfig& target,
                                             const std::string& command,
                                             std::chrono::milliseconds timeout,
                                             std::size_t maximum_output_bytes,
                                             bool preserve_truncated_output) {
  ExecResult result;
  const auto started = Clock::now();
  const DurationTracker duration_tracker{result, started};
  const auto deadline = started + timeout;
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
  std::size_t stdout_remaining = maximum_output_bytes;
  std::size_t stderr_remaining = maximum_output_bytes;
  std::size_t combined_remaining = maximum_output_bytes;
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
        bool& stream_truncated = stream_id == 0 ? result.stdout_truncated : result.stderr_truncated;
        std::size_t* remaining = preserve_truncated_output
            ? &combined_remaining
            : (stream_id == 0 ? &stdout_remaining : &stderr_remaining);
        if (!append_output(&output, buffer.data(), static_cast<std::size_t>(count), remaining)) {
          stream_truncated = true;
          result.output_truncated = true;
          if (!preserve_truncated_output) {
            output_limit_exceeded = true;
            break;
          }
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
    } else if (result.output_truncated && !preserve_truncated_output) {
      fail(&result, "output_limit", "SSH output exceeded the configured 64 KiB per-stream limit");
    } else {
      result.completed = true;
    }
  }
  return result;
}

static ExecResult run_ssh_command_with_timeout_limit(const TargetConfig& target,
                                                     const std::string& command,
                                                     int timeout_seconds,
                                                     int maximum_timeout_seconds) {
  if (timeout_seconds < 1 || timeout_seconds > maximum_timeout_seconds) {
    ExecResult result;
    fail(&result, "invalid_parameters", "The structured SSH operation timeout is invalid");
    return result;
  }
  return run_ssh_command_with_limits(target, command, std::chrono::seconds(timeout_seconds),
                                     kMaxOutputBytes, false);
}

static ExecResult server_exec_operation(const TargetConfig& target,
                                        const ExecRequest& request) {
  if (!target.allow_server_exec) {
    ExecResult result;
    fail(&result, "operation_not_allowed", "server.exec requires a one-shot host authorization");
    return result;
  }
  std::string command;
  const auto quote = [](const std::string& value) {
    std::string quoted = "'";
    for (const char character : value) {
      if (character == '\'') quoted += "'\\''";
      else quoted += character;
    }
    quoted += '\'';
    return quoted;
  };
  if (!request.workdir.empty()) command = "cd " + quote(request.workdir) + " && ";
  if (!request.environment.empty()) {
    command += "env";
    for (const auto& [key, value] : request.environment) {
      command += " " + key + "=" + quote(value);
    }
    command += " /bin/sh -c ";
  } else {
    command += "/bin/sh -c ";
  }
  command += quote(request.command);
  return run_ssh_command_with_limits(target, command,
                                     std::chrono::milliseconds(request.timeout_ms),
                                     request.max_output_bytes, true);
}

static ExecResult run_ssh_command(const TargetConfig& target, const std::string& command,
                                  int timeout_seconds) {
  return run_ssh_command_with_timeout_limit(target, command, timeout_seconds, 30);
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

ExecResult connection_check(const TargetConfig& target, int timeout_seconds) {
  ExecResult result;
  const auto started = Clock::now();
  const DurationTracker duration_tracker{result, started};
  if (target.probe_only || timeout_seconds < 1 || timeout_seconds > 30) {
    fail(&result, "invalid_parameters", "The SSH connection check parameters are invalid");
    return result;
  }
  const auto deadline = started + std::chrono::seconds(timeout_seconds);
  SshResources resources;
  if (!connect_authenticated(target, deadline, &resources, &result)) return result;
  result.stdout_text = "{\"connected\":true}";
  result.exit_code = 0;
  result.completed = true;
  return result;
}

ExecResult system_snapshot(const TargetConfig& target, const std::string& nonce,
                           int timeout_seconds) {
  if (target.probe_only || nonce.size() != 32 || timeout_seconds < 1 || timeout_seconds > 30 ||
      !std::all_of(nonce.begin(), nonce.end(), [](unsigned char byte) {
        return (byte >= '0' && byte <= '9') || (byte >= 'a' && byte <= 'f');
      })) {
    ExecResult result;
    fail(&result, "invalid_parameters", "The system snapshot parameters are invalid");
    return result;
  }
  static constexpr std::pair<std::string_view, std::string_view> commands[] = {
      {"os_detect", "uname -s 2>/dev/null; test $? -eq 0"},
      {"os_release", "cat /etc/os-release 2>/dev/null"},
      {"os_kernel", "uname -r"},
      {"os_arch", "uname -m"},
      {"os_hostname", "hostname"},
      {"cpu_detect", "test -r /proc/stat && echo ok"},
      {"cpu_ticks_a", "head -n1 /proc/stat"},
      {"cpu_wait", "sleep 0.12"},
      {"cpu_ticks_b", "head -n1 /proc/stat"},
      {"cpu_cores", "nproc 2>/dev/null || grep -c ^processor /proc/cpuinfo"},
      {"cpu_model", "grep -m1 'model name' /proc/cpuinfo | cut -d: -f2"},
      {"cpu_load", "cat /proc/loadavg"},
      {"memory_detect", "test -r /proc/meminfo && echo ok"},
      {"memory_info", "cat /proc/meminfo"},
      {"uptime_detect", "test -r /proc/uptime && echo ok"},
      {"uptime_info", "cat /proc/uptime"},
      {"disk_detect", "command -v df >/dev/null && echo ok"},
      {"disk_usage", "df -B1 -P 2>/dev/null"},
      {"network_detect", "test -r /proc/net/dev && echo ok"},
      {"network_info", "cat /proc/net/dev"},
      {"docker_detect", "docker info >/dev/null 2>&1; echo $?"},
      {"docker_info", "docker info >/dev/null 2>&1"},
      {"docker_ps", "docker ps -a --format '{{json .}}' 2>/dev/null"},
  };
  std::string command;
  command.reserve(8192);
  for (const auto& [id, probe] : commands) {
    command += "printf '__YK_SNAPSHOT_" + nonce + "_" + std::string(id) + "_BEGIN__\\n'; ";
    command += probe;
    command += "; yk_snapshot_status=$?; printf '\\n__YK_SNAPSHOT_" + nonce + "_" +
               std::string(id) + "_END_%s__\\n' \"$yk_snapshot_status\"; ";
  }
  return run_ssh_command(target, command, timeout_seconds);
}

ExecResult docker_ps(const TargetConfig& target, bool include_all_containers,
                    int timeout_seconds) {
  if (target.probe_only || timeout_seconds < 1 || timeout_seconds > 30) {
    ExecResult result;
    fail(&result, "invalid_parameters", "The Docker listing parameters are invalid");
    return result;
  }
  const std::string command = include_all_containers
                                  ? "docker ps -a --format '{{json .}}' 2>/dev/null"
                                  : "docker ps --format '{{json .}}' 2>/dev/null";
  return run_ssh_command(target, command, timeout_seconds);
}

ExecResult docker_logs(const TargetConfig& target, const std::string& container,
                       std::uint32_t tail, int timeout_seconds) {
  if (target.probe_only || !valid_container_ref(container) || tail < 1 || tail > 500 ||
      timeout_seconds < 1 || timeout_seconds > 30) {
    ExecResult result;
    fail(&result, "invalid_parameters", "The Docker log parameters are invalid");
    return result;
  }
  const std::string command = "docker logs --tail " + std::to_string(tail) +
                              " --timestamps -- '" + container + "' 2>&1";
  return run_ssh_command(target, command, timeout_seconds);
}

ExecResult docker_inspect(const TargetConfig& target, const std::string& container,
                          int timeout_seconds) {
  if (target.probe_only || !valid_container_ref(container) || timeout_seconds < 1 ||
      timeout_seconds > 30) {
    ExecResult result;
    fail(&result, "invalid_parameters", "The Docker inspect parameters are invalid");
    return result;
  }
  const std::string command = "docker inspect --format '{{json .}}' -- '" + container +
                              "' 2>/dev/null";
  return run_ssh_command(target, command, timeout_seconds);
}

ExecResult systemd_inspect(const TargetConfig& target, const std::string& unit,
                           int timeout_seconds) {
  if (target.probe_only || !valid_service_name(unit) || !unit.ends_with(".service") ||
      timeout_seconds < 1 || timeout_seconds > 30) {
    ExecResult result;
    fail(&result, "invalid_parameters", "The systemd inspect parameters are invalid");
    return result;
  }
  const std::string command =
      "systemctl show --no-pager --property=LoadState,ActiveState,SubState,Description --value -- '" +
      unit + "' 2>&1";
  return run_ssh_command(target, command, timeout_seconds);
}

ExecResult package_inspect(const TargetConfig& target, const std::string& manager,
                           const std::string& package, int timeout_seconds) {
  if (target.probe_only || !valid_package_manager(manager) || !valid_package_ref(package) ||
      timeout_seconds < 1 || timeout_seconds > 30) {
    ExecResult result;
    fail(&result, "invalid_parameters", "The package inspect parameters are invalid");
    return result;
  }
  const std::string package_arg = "'" + package + "'";
  const std::string command = manager == "apt"
      ? "if command -v dpkg-query >/dev/null 2>&1; then dpkg-query --showformat='${Status}\\t${Version}\\n' --show -- " +
            package_arg + " 2>/dev/null || printf '%s\\n' absent; else printf '%s\\n' unsupported; fi"
      : "if command -v rpm >/dev/null 2>&1; then rpm --queryformat='installed\\t%{VERSION}-%{RELEASE}\\n' --query -- " +
            package_arg + " 2>/dev/null || printf '%s\\n' absent; else printf '%s\\n' unsupported; fi";
  return run_ssh_command(target, command, timeout_seconds);
}

ExecResult docker_restart(const TargetConfig& target, const std::string& container,
                          std::uint32_t grace_period_seconds, int timeout_seconds) {
  if (target.probe_only || !valid_container_ref(container) || grace_period_seconds < 1 ||
      grace_period_seconds > 120 || timeout_seconds < 1 || timeout_seconds > 30) {
    ExecResult result;
    fail(&result, "invalid_parameters", "The Docker restart parameters are invalid");
    return result;
  }
  const std::string command = "docker restart --time " +
                              std::to_string(grace_period_seconds) + " -- '" + container +
                              "' 2>&1";
  return run_ssh_command(target, command, timeout_seconds);
}

ExecResult systemd_restart(const TargetConfig& target, const std::string& unit,
                           int timeout_seconds) {
  if (target.probe_only || !valid_service_name(unit) || !unit.ends_with(".service") ||
      timeout_seconds < 1 || timeout_seconds > 120) {
    ExecResult result;
    fail(&result, "invalid_parameters", "The systemd restart parameters are invalid");
    return result;
  }
  const std::string command =
      "systemctl restart --no-ask-password --wait -- '" + unit + "' 2>&1";
  return run_ssh_command_with_timeout_limit(target, command, timeout_seconds, 120);
}

ExecResult package_install(const TargetConfig& target, const std::string& manager,
                           const std::string& package, const std::string& version,
                           int timeout_seconds) {
  if (target.probe_only || !valid_package_manager(manager) || !valid_package_ref(package) ||
      (!version.empty() && !valid_package_version(version)) || timeout_seconds < 1 ||
      timeout_seconds > 600) {
    ExecResult result;
    fail(&result, "invalid_parameters", "The package install parameters are invalid");
    return result;
  }
  std::string specification = package;
  if (!version.empty()) specification += manager == "apt" ? "=" + version : "-" + version;
  const std::string package_arg = "'" + specification + "'";
  const std::string command = manager == "apt"
      ? "DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-install-recommends --no-remove -- " +
            package_arg + " 2>&1"
      : "dnf install --assumeyes --setopt=install_weak_deps=False -- " + package_arg + " 2>&1";
  return run_ssh_command_with_timeout_limit(target, command, timeout_seconds, 600);
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

ExecResult server_logs(const TargetConfig& target,
                       std::optional<std::uint32_t> since_seconds,
                       const std::string& unit, int timeout_seconds) {
  if ((since_seconds && (*since_seconds < 1 || *since_seconds > 604800)) ||
      (!unit.empty() && (!valid_service_name(unit) || !unit.ends_with(".service")))) {
    ExecResult result;
    fail(&result, "invalid_parameters", "The bounded server log parameters are invalid");
    return result;
  }
  std::string command =
      "if command -v journalctl >/dev/null 2>&1; then journalctl -n 500 --no-pager -o short-iso";
  if (since_seconds) command += " --since '-" + std::to_string(*since_seconds) + " seconds'";
  if (!unit.empty()) command += " --unit '" + unit + "'";
  command +=
      " 2>/dev/null; if [ $? -eq 0 ]; then printf '\\n__YUKINAL_SOURCE__=journalctl\\n'; exit 0; fi; fi; ";
  if (!since_seconds && unit.empty()) {
    command +=
        "for file in /var/log/syslog /var/log/messages; do if [ -r \"$file\" ]; then "
        "tail -n 500 \"$file\"; if [ $? -eq 0 ]; then case \"$file\" in "
        "/var/log/syslog) printf '\\n__YUKINAL_SOURCE__=syslog\\n' ;; "
        "/var/log/messages) printf '\\n__YUKINAL_SOURCE__=messages\\n' ;; esac; exit 0; fi; fi; done; ";
  }
  command += "printf '__YUKINAL_SOURCE__=unavailable\\n'";
  return run_ssh_command(target, command, timeout_seconds);
}

ExecResult host_key_probe(const TargetConfig& target, int timeout_seconds) {
  ExecResult result;
  const auto started = Clock::now();
  const DurationTracker duration_tracker{result, started};
  if (!target.probe_only || timeout_seconds < 1 || timeout_seconds > 30) {
    fail(&result, "invalid_parameters", "The host-key probe target or timeout is invalid");
    return result;
  }

  if (initialize_libssh2() != 0) {
    fail(&result, "ssh_unavailable", "SSH library initialization failed");
    return result;
  }

  const auto deadline = started + std::chrono::seconds(timeout_seconds);
  SocketConnection connection = connect_socket(target, deadline);
  SshResources resources;
  resources.socket_ = connection.socket;
  if (resources.socket_ == kInvalidSocket) {
    switch (connection.failure) {
      case ConnectFailure::timeout:
        fail(&result, "timeout", "SSH host-key probe timed out");
        break;
      case ConnectFailure::resolver_busy:
        fail(&result, "resolver_busy", "DNS resolution capacity is temporarily exhausted");
        break;
      case ConnectFailure::resolver_unavailable:
        fail(&result, "resolver_unavailable", "Could not start bounded DNS resolution");
        break;
      case ConnectFailure::proxy_auth_required:
        fail(&result, "proxy_auth_required", "The configured HTTP proxy credential is missing or was rejected");
        break;
      case ConnectFailure::proxy_connect_failed:
        fail(&result, "proxy_connect_failed", "Could not establish a tunnel through the configured proxy");
        break;
      case ConnectFailure::proxy_tls_failed:
        fail(&result, "proxy_tls_failed", "Could not establish a verified TLS connection to the HTTP proxy");
        break;
      default:
        fail(&result, "connection_failed", "Could not connect to the SSH host for probing");
        break;
    }
    return result;
  }

  resources.proxy_tls_ = std::move(connection.proxy_tls);
  resources.session_context_.proxy_tls = resources.proxy_tls_.get();
  resources.session_context_.socket = resources.socket_;
  resources.session_context_.deadline = deadline;
  if (!resources.initialize_session()) {
    fail(&result, "ssh_unavailable", "Could not create an SSH session");
    return result;
  }
  libssh2_session_set_blocking(resources.session_, 1);
  if (!set_session_timeout(resources.session_, deadline)) {
    fail(&result, "timeout", "SSH host-key probe timed out");
    return result;
  }
  if (libssh2_session_handshake(resources.session_, static_cast<libssh2_socket_t>(resources.socket_)) != 0) {
    const bool timed_out = remaining_ms(deadline) == 0 ||
                           libssh2_session_last_errno(resources.session_) == LIBSSH2_ERROR_TIMEOUT;
    fail(&result, timed_out ? "timeout" : "ssh_handshake_failed",
         timed_out ? "SSH host-key probe timed out" : "SSH host-key handshake failed");
    return result;
  }

  std::size_t host_key_length = 0;
  const char* host_key =
      libssh2_session_hostkey(resources.session_, &host_key_length, nullptr);
  if (!host_key || host_key_length == 0) {
    fail(&result, "host_key_unavailable", "The SSH server did not present a host key");
    return result;
  }
  result.stdout_text = detail::fingerprint_host_key_blob(
      std::string_view(host_key, host_key_length));
  if (result.stdout_text.empty()) {
    fail(&result, "host_key_unavailable", "The SSH server did not present a supported host key");
    return result;
  }
  result.exit_code = 0;
  result.completed = true;
  return result;
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

static ExecResult sftp_stat_impl(const TargetConfig& target, const std::string& path,
                                 int timeout_seconds, bool optional) {
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
    const unsigned long sftp_error = libssh2_sftp_last_error(resources.sftp_);
    if (optional && (sftp_error == LIBSSH2_FX_NO_SUCH_FILE ||
                     sftp_error == LIBSSH2_FX_NO_SUCH_PATH)) {
      result.stdout_text = "{\"exists\":false}";
      result.exit_code = 0;
      result.completed = true;
      return result;
    }
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
  if (optional) output["exists"] = true;
  result.stdout_text = output.dump();
  result.exit_code = 0;
  result.completed = true;
  return result;
}

ExecResult sftp_stat(const TargetConfig& target, const std::string& path, int timeout_seconds) {
  return sftp_stat_impl(target, path, timeout_seconds, false);
}

ExecResult sftp_lstat_optional(const TargetConfig& target, const std::string& path,
                               int timeout_seconds) {
  return sftp_stat_impl(target, path, timeout_seconds, true);
}

ExecResult sftp_create_dir(const TargetConfig& target, const std::string& path,
                           int timeout_seconds) {
  ExecResult result;
  const auto started = Clock::now();
  const DurationTracker duration_tracker{result, started};
  if (path.empty() || path.size() > 4096 || path.find('\0') != std::string::npos ||
      timeout_seconds < 1 || timeout_seconds > 30) {
    fail(&result, "invalid_parameters", "The SFTP directory creation parameters are invalid");
    return result;
  }

  const auto deadline = started + std::chrono::seconds(timeout_seconds);
  SshResources resources;
  if (!connect_sftp(target, deadline, &resources, &result)) return result;
  if (!set_session_timeout(resources.session_, deadline)) {
    fail(&result, "timeout", "SFTP directory creation timed out");
    return result;
  }
  if (libssh2_sftp_mkdir_ex(resources.sftp_, path.data(),
                            static_cast<unsigned int>(path.size()), 0755) != 0) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(&result, timed_out ? "timeout" : "sftp_error",
         timed_out ? "SFTP directory creation timed out" : "Could not create the remote directory");
    return result;
  }
  result.stdout_text = "{\"created\":true}";
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
    set_session_timeout(resources.session_, Clock::now() + std::chrono::milliseconds(1000));
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
    if (!set_session_timeout(resources.session_, deadline)) {
      set_session_timeout(resources.session_, Clock::now() + std::chrono::milliseconds(1000));
    }
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
    set_session_timeout(resources_->session_, Clock::now() + std::chrono::milliseconds(1000));
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
    if (!set_session_timeout(resources.session_, deadline)) {
      set_session_timeout(resources.session_, Clock::now() + std::chrono::milliseconds(1000));
    }
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
  if (!set_session_timeout(resources.session_, deadline)) {
    set_session_timeout(resources.session_, Clock::now() + std::chrono::milliseconds(1000));
  }
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

namespace {

void append_sftp_u32(std::vector<unsigned char>* output, std::uint32_t value) {
  output->push_back(static_cast<unsigned char>((value >> 24) & 0xff));
  output->push_back(static_cast<unsigned char>((value >> 16) & 0xff));
  output->push_back(static_cast<unsigned char>((value >> 8) & 0xff));
  output->push_back(static_cast<unsigned char>(value & 0xff));
}

std::uint32_t read_sftp_u32(const unsigned char* data) {
  return (static_cast<std::uint32_t>(data[0]) << 24) |
         (static_cast<std::uint32_t>(data[1]) << 16) |
         (static_cast<std::uint32_t>(data[2]) << 8) |
         static_cast<std::uint32_t>(data[3]);
}

void append_sftp_string(std::vector<unsigned char>* output, std::string_view value) {
  append_sftp_u32(output, static_cast<std::uint32_t>(value.size()));
  output->insert(output->end(), value.begin(), value.end());
}

bool read_sftp_string(const std::vector<unsigned char>& packet, std::size_t* offset,
                      std::string_view* value) {
  if (!offset || !value || *offset > packet.size() || packet.size() - *offset < 4) return false;
  const std::uint32_t length = read_sftp_u32(packet.data() + *offset);
  *offset += 4;
  if (length > packet.size() - *offset) return false;
  *value = std::string_view(reinterpret_cast<const char*>(packet.data() + *offset), length);
  *offset += length;
  return true;
}

bool write_sftp_channel(SshResources* resources,
                        const std::vector<unsigned char>& bytes,
                        Clock::time_point deadline) {
  std::size_t offset = 0;
  while (offset < bytes.size()) {
    if (!set_session_timeout(resources->session_, deadline)) return false;
    const auto count = libssh2_channel_write_ex(
        resources->channel_, 0, reinterpret_cast<const char*>(bytes.data() + offset),
        bytes.size() - offset);
    if (count > 0) {
      offset += static_cast<std::size_t>(count);
    } else if (count == LIBSSH2_ERROR_EAGAIN &&
               wait_for_ssh(resources->socket_, resources->session_, deadline)) {
      continue;
    } else {
      return false;
    }
  }
  return true;
}

bool read_sftp_channel_exact(SshResources* resources, unsigned char* output,
                             std::size_t size, Clock::time_point deadline) {
  std::size_t offset = 0;
  while (offset < size) {
    if (!set_session_timeout(resources->session_, deadline)) return false;
    const auto count = libssh2_channel_read_ex(
        resources->channel_, 0, reinterpret_cast<char*>(output + offset), size - offset);
    if (count > 0) {
      offset += static_cast<std::size_t>(count);
    } else if ((count == LIBSSH2_ERROR_EAGAIN || count == 0) &&
               !libssh2_channel_eof(resources->channel_) &&
               wait_for_ssh(resources->socket_, resources->session_, deadline)) {
      continue;
    } else {
      return false;
    }
  }
  return true;
}

bool read_sftp_channel_packet(SshResources* resources,
                              std::vector<unsigned char>* packet,
                              Clock::time_point deadline) {
  std::array<unsigned char, 4> length_bytes{};
  if (!read_sftp_channel_exact(resources, length_bytes.data(), length_bytes.size(), deadline)) {
    return false;
  }
  const std::uint32_t length = read_sftp_u32(length_bytes.data());
  if (length == 0 || length > 64 * 1024) return false;
  packet->resize(length);
  return read_sftp_channel_exact(resources, packet->data(), packet->size(), deadline);
}

}  // namespace

ExecResult sftp_publish_no_replace(const TargetConfig& target, const std::string& source,
                                   const std::string& destination, int timeout_seconds) {
  ExecResult result;
  const auto started = Clock::now();
  const DurationTracker duration_tracker{result, started};
  if (source.empty() || destination.empty() || source.size() > 4096 ||
      destination.size() > 4096 || source.front() != '/' || destination.front() != '/' ||
      source.find_first_of("\r\n") != std::string::npos || source.find('\0') != std::string::npos ||
      destination.find_first_of("\r\n") != std::string::npos ||
      destination.find('\0') != std::string::npos || source.back() == '/' ||
      destination.back() == '/' || source == destination ||
      timeout_seconds < 1 || timeout_seconds > 30) {
    fail(&result, "invalid_parameters", "The SFTP no-replace publication paths are invalid");
    return result;
  }
  const auto source_parent = source.substr(0, source.find_last_of('/'));
  const auto destination_parent = destination.substr(0, destination.find_last_of('/'));
  const std::string normalized_source_parent = source_parent.empty() ? "/" : source_parent;
  const std::string normalized_destination_parent =
      destination_parent.empty() ? "/" : destination_parent;
  if (normalized_source_parent != normalized_destination_parent) {
    fail(&result, "invalid_parameters", "The SFTP no-replace publication paths must be siblings");
    return result;
  }

  const auto deadline = started + std::chrono::seconds(timeout_seconds);
  SshResources resources;
  if (!connect_authenticated(target, deadline, &resources, &result)) return result;
  resources.channel_ = libssh2_channel_open_session(resources.session_);
  if (!resources.channel_) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(&result, timed_out ? "timeout" : "sftp_error",
         timed_out ? "Starting the SFTP publication timed out"
                   : "Could not open the SFTP publication channel");
    return result;
  }
  if (!set_session_timeout(resources.session_, deadline) ||
      libssh2_channel_process_startup(resources.channel_, "subsystem", 9, "sftp", 4) != 0) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(&result, timed_out ? "timeout" : "sftp_error",
         timed_out ? "Starting the SFTP publication timed out"
                   : "Could not start the SFTP subsystem for publication");
    return result;
  }
  if (!set_nonblocking(resources.socket_, true)) {
    fail(&result, "sftp_error", "Could not configure the SFTP publication channel");
    return result;
  }
  libssh2_session_set_blocking(resources.session_, 0);
  libssh2_channel_set_blocking(resources.channel_, 0);

  std::vector<unsigned char> init_body{1};  // SSH_FXP_INIT
  append_sftp_u32(&init_body, 3);
  std::vector<unsigned char> init_frame;
  append_sftp_u32(&init_frame, static_cast<std::uint32_t>(init_body.size()));
  init_frame.insert(init_frame.end(), init_body.begin(), init_body.end());
  if (!write_sftp_channel(&resources, init_frame, deadline)) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(&result, timed_out ? "timeout" : "sftp_error",
         timed_out ? "Negotiating the SFTP publication timed out"
                   : "Could not initialize the SFTP publication channel");
    return result;
  }

  std::vector<unsigned char> packet;
  if (!read_sftp_channel_packet(&resources, &packet, deadline)) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(&result, timed_out ? "timeout" : "sftp_error",
         timed_out ? "Negotiating the SFTP publication timed out"
                   : "The SFTP server returned an invalid version packet");
    return result;
  }
  if (packet.size() < 5 || packet[0] != 2 || read_sftp_u32(packet.data() + 1) < 3) {
    fail(&result, "replace_unsupported", "The SFTP server did not negotiate a supported protocol version");
    return result;
  }
  bool hardlink_v1 = false;
  std::size_t extension_offset = 5;
  while (extension_offset < packet.size()) {
    std::string_view name;
    std::string_view version;
    if (!read_sftp_string(packet, &extension_offset, &name) ||
        !read_sftp_string(packet, &extension_offset, &version)) {
      fail(&result, "sftp_error", "The SFTP server returned malformed extension metadata");
      return result;
    }
    if (name == "hardlink@openssh.com" && version == "1") hardlink_v1 = true;
  }
  if (!hardlink_v1) {
    fail(&result, "replace_unsupported",
         "The SFTP server did not negotiate hardlink@openssh.com version 1");
    return result;
  }

  std::vector<unsigned char> request_body{200};  // SSH_FXP_EXTENDED
  append_sftp_u32(&request_body, 1);
  append_sftp_string(&request_body, "hardlink@openssh.com");
  append_sftp_string(&request_body, source);
  append_sftp_string(&request_body, destination);
  std::vector<unsigned char> request_frame;
  append_sftp_u32(&request_frame, static_cast<std::uint32_t>(request_body.size()));
  request_frame.insert(request_frame.end(), request_body.begin(), request_body.end());
  if (!write_sftp_channel(&resources, request_frame, deadline) ||
      !read_sftp_channel_packet(&resources, &packet, deadline)) {
    const bool timed_out = remaining_ms(deadline) == 0;
    fail(&result, timed_out ? "timeout" : "sftp_error",
         timed_out ? "Publishing the remote file timed out"
                   : "The SFTP server did not confirm file publication");
    return result;
  }
  if (packet.size() < 9 || packet[0] != 101 || read_sftp_u32(packet.data() + 1) != 1) {
    fail(&result, "sftp_error", "The SFTP server returned an invalid publication response");
    return result;
  }
  const std::uint32_t status = read_sftp_u32(packet.data() + 5);
  if (status == LIBSSH2_FX_FILE_ALREADY_EXISTS) {
    fail(&result, "already_exists", "The remote destination already exists");
    return result;
  }
  if (status != LIBSSH2_FX_OK) {
    fail(&result, "sftp_error", "The SFTP server refused no-replace publication");
    return result;
  }
  result.stdout_text = "{\"published\":true}";
  result.exit_code = 0;
  result.completed = true;
  return result;
}

ExecResult run_operation(const TargetConfig& target, const ExecRequest& request) {
  if (target.probe_only) {
    if (request.operation != "host_key_probe") {
      ExecResult result;
      fail(&result, "operation_not_allowed", "Probe-only targets allow only host_key_probe");
      return result;
    }
    return host_key_probe(target, request.timeout_seconds);
  }
  if (request.operation == "host_key_probe") {
    ExecResult result;
    fail(&result, "operation_not_allowed", "host_key_probe requires a probe-only target");
    return result;
  }
  if (request.operation == "service_status") {
    return service_status(target, request.service, request.timeout_seconds);
  }
  if (request.operation == "server_exec") {
    return server_exec_operation(target, request);
  }
  if (request.operation == "connection_check") {
    return connection_check(target, request.timeout_seconds);
  }
  if (request.operation == "system_snapshot") {
    return system_snapshot(target, request.nonce, request.timeout_seconds);
  }
  if (request.operation == "docker_ps") {
    return docker_ps(target, request.include_all_containers, request.timeout_seconds);
  }
  if (request.operation == "docker_logs") {
    return docker_logs(target, request.container, request.tail, request.timeout_seconds);
  }
  if (request.operation == "docker_inspect") {
    return docker_inspect(target, request.container, request.timeout_seconds);
  }
  if (request.operation == "systemd_inspect") {
    return systemd_inspect(target, request.unit, request.timeout_seconds);
  }
  if (request.operation == "package_inspect") {
    return package_inspect(target, request.package_manager, request.package,
                           request.timeout_seconds);
  }
  if (request.operation == "docker_restart") {
    return docker_restart(target, request.container, request.grace_period_seconds,
                          request.timeout_seconds);
  }
  if (request.operation == "systemd_restart") {
    return systemd_restart(target, request.unit, request.timeout_seconds);
  }
  if (request.operation == "package_install") {
    return package_install(target, request.package_manager, request.package,
                           request.package_version, request.timeout_seconds);
  }
  if (request.operation == "service_inventory") {
    return service_inventory(target, request.timeout_seconds);
  }
  if (request.operation == "server_logs") {
    return server_logs(target, request.since_seconds, request.unit, request.timeout_seconds);
  }
  if (request.operation == "sftp_list") {
    return sftp_list(target, request.path, request.timeout_seconds);
  }
  if (request.operation == "sftp_stat") {
    return sftp_stat(target, request.path, request.timeout_seconds);
  }
  if (request.operation == "sftp_lstat_optional") {
    return sftp_lstat_optional(target, request.path, request.timeout_seconds);
  }
  if (request.operation == "sftp_create_dir") {
    return sftp_create_dir(target, request.path, request.timeout_seconds);
  }
  if (request.operation == "sftp_publish_no_replace") {
    return sftp_publish_no_replace(target, request.source_path, request.destination_path,
                                   request.timeout_seconds);
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

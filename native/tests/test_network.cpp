#include "test_support.hpp"

#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <winsock2.h>
#include <ws2tcpip.h>
#else
#include <netdb.h>
#include <arpa/inet.h>
#include <sys/socket.h>
#include <unistd.h>
#endif

#include "ssh_network.hpp"
#include "ssh_timeouts.hpp"
#include "yukinal/ssh.hpp"

#include <algorithm>
#include <array>
#include <atomic>
#include <chrono>
#include <memory>
#include <thread>
#include <string>
#include <string_view>
#include <libssh2.h>

using yukinal::ssh_service::detail::AddressResolution;
using yukinal::ssh_service::detail::resolve_address_list;

YUKINAL_TEST(probe_only_target_rejects_all_authenticated_operations) {
  yukinal::ssh_service::TargetConfig target;
  target.target_id = "probe-target";
  target.host = "example.test";
  target.port = 22;
  target.username = "ssh_probe";
  target.probe_only = true;

  yukinal::ssh_service::ExecRequest request;
  request.operation = "service_status";
  request.service = "nginx.service";
  request.timeout_seconds = 5;
  const auto result = yukinal::ssh_service::run_operation(target, request);

  YUKINAL_REQUIRE(!result.completed);
  YUKINAL_REQUIRE(result.error_code == "operation_not_allowed");
}

YUKINAL_TEST(host_key_probe_rejects_regular_authenticated_target) {
  yukinal::ssh_service::TargetConfig target;
  target.target_id = "regular-target";
  target.host = "example.test";
  target.port = 22;
  target.username = "ops";

  yukinal::ssh_service::ExecRequest request;
  request.operation = "host_key_probe";
  request.timeout_seconds = 5;
  const auto result = yukinal::ssh_service::run_operation(target, request);

  YUKINAL_REQUIRE(!result.completed);
  YUKINAL_REQUIRE(result.error_code == "operation_not_allowed");
}

YUKINAL_TEST(dns_resolution_obeys_the_operation_deadline) {
  using Clock = std::chrono::steady_clock;
  auto calls = std::make_shared<std::atomic_int>(0);
  const yukinal::ssh_service::detail::GetAddrInfo slow_resolver =
      [calls](const char*, const char*, const addrinfo*, addrinfo**) {
        if (calls->fetch_add(1) == 0) return EAI_NONAME;
        std::this_thread::sleep_for(std::chrono::milliseconds(500));
        return EAI_NONAME;
      };

  addrinfo hints{};
  hints.ai_family = AF_UNSPEC;
  hints.ai_socktype = SOCK_STREAM;
  addrinfo* addresses = nullptr;
  const auto started = Clock::now();
  const auto status = resolve_address_list("slow.invalid", "22", hints,
      started + std::chrono::milliseconds(25), &addresses, slow_resolver);
  const auto elapsed = std::chrono::duration_cast<std::chrono::milliseconds>(Clock::now() - started);

  YUKINAL_REQUIRE(status == AddressResolution::timed_out);
  YUKINAL_REQUIRE(addresses == nullptr);
  YUKINAL_REQUIRE(elapsed < std::chrono::milliseconds(200));

  // Let the deliberately slow resolver finish so the detached test worker is
  // quiescent before the test executable exits.
  std::this_thread::sleep_for(std::chrono::milliseconds(520));
}


namespace {

#ifdef _WIN32
using TestSocket = SOCKET;
constexpr TestSocket kInvalidTestSocket = INVALID_SOCKET;
void close_test_socket(TestSocket value) { if (value != kInvalidTestSocket) closesocket(value); }
#else
using TestSocket = int;
constexpr TestSocket kInvalidTestSocket = -1;
void close_test_socket(TestSocket value) { if (value != kInvalidTestSocket) close(value); }
#endif

void append_uint32(std::string& output, std::uint32_t value) {
  for (const auto shift : {24, 16, 8, 0}) output.push_back(static_cast<char>(value >> shift));
}

// A loopback-only SSH banner and KEXINIT with an intentionally disjoint KEX.
// No authentication is implemented and no real target configuration is loaded.
class NoCommonKexServer {
 public:
  explicit NoCommonKexServer(bool stall_handshake = false) {
#ifdef _WIN32
    WSADATA data{};
    YUKINAL_REQUIRE(WSAStartup(MAKEWORD(2, 2), &data) == 0);
#endif
    listener_ = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    YUKINAL_REQUIRE(listener_ != kInvalidTestSocket);
    sockaddr_in endpoint{};
    endpoint.sin_family = AF_INET;
    endpoint.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (bind(listener_, reinterpret_cast<sockaddr*>(&endpoint), sizeof(endpoint)) != 0 ||
        listen(listener_, 1) != 0) {
      close_test_socket(listener_);
      listener_ = kInvalidTestSocket;
      throw std::runtime_error("could not bind the local handshake test server");
    }
#ifdef _WIN32
    int length = sizeof(endpoint);
#else
    socklen_t length = sizeof(endpoint);
#endif
    YUKINAL_REQUIRE(getsockname(listener_, reinterpret_cast<sockaddr*>(&endpoint), &length) == 0);
    port_ = ntohs(endpoint.sin_port);
    worker_ = std::thread([this, stall_handshake] {
      const auto client = accept(listener_, nullptr, nullptr);
      if (client == kInvalidTestSocket) return;
#ifdef _WIN32
      const DWORD timeout = 2000;
#else
      const timeval timeout{2, 0};
#endif
      setsockopt(client, SOL_SOCKET, SO_RCVTIMEO,
                 reinterpret_cast<const char*>(&timeout), sizeof(timeout));
      setsockopt(client, SOL_SOCKET, SO_SNDTIMEO,
                 reinterpret_cast<const char*>(&timeout), sizeof(timeout));
      std::string payload(1, static_cast<char>(20));  // SSH_MSG_KEXINIT
      payload.append(16, '\0');
      for (const auto method : {"no-common-kex@yukinal.test", "ssh-ed25519", "aes256-ctr",
                                "aes256-ctr", "hmac-sha2-256", "hmac-sha2-256",
                                "none", "none", "", ""}) {
        const std::string_view value(method);
        append_uint32(payload, static_cast<std::uint32_t>(value.size()));
        payload += value;
      }
      payload.push_back('\0');
      append_uint32(payload, 0);
      auto padding = 8 - ((payload.size() + 5) % 8);
      if (padding < 4) padding += 8;
      std::string packet = "SSH-2.0-Yukinal-local-KEX-test\r\n";
      append_uint32(packet, static_cast<std::uint32_t>(1 + payload.size() + padding));
      packet.push_back(static_cast<char>(padding));
      packet += payload;
      packet.append(padding, '\0');
      if (stall_handshake) packet = "SSH-2.0-Yukinal-local-stall-test\r\n";
      std::size_t sent = 0;
      while (sent < packet.size()) {
        const auto count = send(client, packet.data() + sent,
                                static_cast<int>(packet.size() - sent), 0);
        if (count <= 0) break;
        sent += static_cast<std::size_t>(count);
      }
      std::array<char, 4096> input{};
      while (recv(client, input.data(), static_cast<int>(input.size()), 0) > 0) {}
      close_test_socket(client);
    });
  }
  ~NoCommonKexServer() {
    close_test_socket(listener_);
    if (worker_.joinable()) worker_.join();
#ifdef _WIN32
    WSACleanup();
#endif
  }
  int port() const { return port_; }

 private:
  TestSocket listener_ = kInvalidTestSocket;
  int port_ = 0;
  std::thread worker_;
};

}  // namespace

YUKINAL_TEST(host_key_probe_reports_local_handshake_error_details) {
  NoCommonKexServer server;
  yukinal::ssh_service::TargetConfig target;
  target.target_id = "local-probe-test";
  target.host = "127.0.0.1";
  target.port = server.port();
  target.username = "ssh_probe";
  target.probe_only = true;
  const auto result = yukinal::ssh_service::host_key_probe(target, 3);
  YUKINAL_REQUIRE(!result.completed);
  YUKINAL_REQUIRE(result.error_code == "ssh_handshake_failed");
  YUKINAL_REQUIRE(result.error_message.find("libssh2 -") != std::string::npos);
  YUKINAL_REQUIRE(result.error_message.find("Unable to exchange encryption keys") != std::string::npos);
  YUKINAL_REQUIRE(result.error_message.size() < 512);
  YUKINAL_REQUIRE(std::none_of(result.error_message.begin(), result.error_message.end(),
                              [](unsigned char value) { return value < 0x20 || value > 0x7e; }));
}

YUKINAL_TEST(connection_check_reports_local_handshake_error_before_authentication) {
  NoCommonKexServer server;
  yukinal::ssh_service::TargetConfig target;
  target.target_id = "local-connect-test";
  target.host = "127.0.0.1";
  target.port = server.port();
  target.username = "ops";
  target.host_key_fingerprint = "SHA256:" + std::string(43, 'A');
  const auto result = yukinal::ssh_service::connection_check(target, 3);
  YUKINAL_REQUIRE(!result.completed);
  YUKINAL_REQUIRE(result.error_code == "ssh_handshake_failed");
  YUKINAL_REQUIRE(result.error_message.find("SSH handshake failed (libssh2 -") != std::string::npos);
  YUKINAL_REQUIRE(result.error_message.find("Unable to exchange encryption keys") != std::string::npos);
}

#ifdef YUKINAL_EXPECT_OPENSSL_BACKEND
YUKINAL_TEST(openssl_backend_exposes_modern_ssh_algorithms) {
  YUKINAL_REQUIRE(libssh2_init(0) == 0);
  auto* session = libssh2_session_init();
  YUKINAL_REQUIRE(session != nullptr);
  const auto supports = [session](int method, std::string_view expected) {
    const char** algorithms = nullptr;
    const auto count = libssh2_session_supported_algs(session, method, &algorithms);
    bool found = false;
    for (int index = 0; index < count; ++index) found = found || expected == algorithms[index];
    libssh2_free(session, algorithms);
    return found;
  };
  const bool curve25519 = supports(LIBSSH2_METHOD_KEX, "curve25519-sha256");
  const bool ed25519 = supports(LIBSSH2_METHOD_HOSTKEY, "ssh-ed25519");
  const bool aes_gcm = supports(LIBSSH2_METHOD_CRYPT_CS, "aes256-gcm@openssh.com");
  libssh2_session_free(session);
  YUKINAL_REQUIRE(curve25519);
  YUKINAL_REQUIRE(ed25519);
  YUKINAL_REQUIRE(aes_gcm);
}
#endif

YUKINAL_TEST(sftp_metadata_deadline_allows_slow_auth_within_a_fixed_total_limit) {
  using Budget = yukinal::ssh_service::detail::SftpMetadataDeadline;
  using Phase = Budget::Phase;
  using namespace std::chrono_literals;
  const Budget::Clock::time_point start{};
  Budget budget(start, 30s);
  YUKINAL_REQUIRE(yukinal::ssh_service::kSftpMetadataTimeoutSeconds == 30);
  YUKINAL_REQUIRE(budget.enter(Phase::connection, start) == start + 10s);
  YUKINAL_REQUIRE(budget.enter(Phase::handshake, start + 2s) == start + 12s);
  // Authentication begins after connection and handshake. A slow server-side
  // lookup may finish after the old 10-second total, while still bounded by
  // the authentication allowance and the unchanged 30-second total.
  const auto auth_deadline = budget.enter(Phase::authentication, start + 9s);
  YUKINAL_REQUIRE(start + 16s < auth_deadline);
  YUKINAL_REQUIRE(auth_deadline == start + 24s);
  YUKINAL_REQUIRE(budget.enter(Phase::startup, start + 16s) == start + 21s);
  const auto operation_deadline = budget.enter(Phase::operation, start + 17s);
  YUKINAL_REQUIRE(operation_deadline == start + 27s);
  // Packet progress is not a reason to extend a phase deadline.
  YUKINAL_REQUIRE(budget.enter(Phase::operation, start + 26s) == operation_deadline);
  YUKINAL_REQUIRE(budget.overall() == start + 30s);
}

YUKINAL_TEST(sftp_metadata_deadline_caps_every_phase_by_the_remaining_total) {
  using Budget = yukinal::ssh_service::detail::SftpMetadataDeadline;
  using Phase = Budget::Phase;
  using namespace std::chrono_literals;
  const Budget::Clock::time_point start{};
  Budget budget(start, 30s);
  YUKINAL_REQUIRE(budget.enter(Phase::connection, start + 9s) == start + 19s);
  YUKINAL_REQUIRE(budget.enter(Phase::handshake, start + 18s) == start + 28s);
  YUKINAL_REQUIRE(budget.enter(Phase::authentication, start + 24s) == start + 30s);
  YUKINAL_REQUIRE(budget.enter(Phase::startup, start + 29s) == start + 30s);
  YUKINAL_REQUIRE(budget.enter(Phase::operation, start + 31s) == start + 30s);

  Budget short_request(start, 1s);
  for (const auto phase : {Phase::host_key, Phase::connection, Phase::handshake,
                          Phase::authentication, Phase::startup, Phase::operation}) {
    YUKINAL_REQUIRE(short_request.enter(phase, start) == start + 1s);
  }
}

YUKINAL_TEST(sftp_metadata_and_publication_return_at_the_requested_handshake_deadline) {
  using namespace std::chrono_literals;
  for (const bool publish : {false, true}) {
    NoCommonKexServer server(true);  // Loopback banner only, no KEX or auth.
    yukinal::ssh_service::TargetConfig target;
    target.target_id = "local-sftp-timeout-test";
    target.host = "127.0.0.1";
    target.port = server.port();
    target.username = "local-fixture";
    target.host_key_fingerprint = "SHA256:" + std::string(43, 'A');
    yukinal::ssh_service::ScopedCredentialOverride no_credentials({});
    const auto started = std::chrono::steady_clock::now();
    const auto result = publish
        ? yukinal::ssh_service::sftp_publish_no_replace(target, "/tmp/local.part", "/tmp/local", 1)
        : yukinal::ssh_service::sftp_lstat_optional(target, "/tmp/local.part", 1);
    const auto elapsed = std::chrono::steady_clock::now() - started;
    YUKINAL_REQUIRE(!result.completed);
    YUKINAL_REQUIRE(result.error_code == "timeout");
    YUKINAL_REQUIRE(result.error_message.find("SSH handshake timed out") != std::string::npos);
    YUKINAL_REQUIRE(result.error_message.find("SFTP SSH 握手阶段") != std::string::npos);
    YUKINAL_REQUIRE(elapsed >= 700ms);
    YUKINAL_REQUIRE(elapsed < 2500ms);
    YUKINAL_REQUIRE(result.duration_ms >= 700);
    YUKINAL_REQUIRE(result.duration_ms < 2500);
  }
}

YUKINAL_TEST(sftp_metadata_and_publication_keep_the_existing_thirty_second_policy_cap) {
  yukinal::ssh_service::TargetConfig target;
  for (const auto timeout : {0, 31}) {
    for (const auto& result : {
          yukinal::ssh_service::sftp_list(target, "/tmp", timeout),
          yukinal::ssh_service::sftp_lstat_optional(target, "/tmp/local.part", timeout),
          yukinal::ssh_service::sftp_create_dir(target, "/tmp/local", timeout),
          yukinal::ssh_service::sftp_remove_file(target, "/tmp/local.part", timeout),
          yukinal::ssh_service::sftp_publish_no_replace(target, "/tmp/local.part", "/tmp/local", timeout)}) {
      YUKINAL_REQUIRE(!result.completed);
      YUKINAL_REQUIRE(result.error_code == "invalid_parameters");
    }
  }
}

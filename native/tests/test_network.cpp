#include "test_support.hpp"

#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <winsock2.h>
#include <ws2tcpip.h>
#else
#include <netdb.h>
#endif

#include "ssh_network.hpp"
#include "yukinal/ssh.hpp"

#include <atomic>
#include <chrono>
#include <memory>
#include <thread>

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

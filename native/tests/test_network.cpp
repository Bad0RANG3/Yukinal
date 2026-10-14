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

#include <atomic>
#include <chrono>
#include <memory>
#include <thread>

using yukinal::ssh_service::detail::AddressResolution;
using yukinal::ssh_service::detail::resolve_address_list;

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

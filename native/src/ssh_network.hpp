#pragma once

#include <chrono>
#include <functional>
#include <string>

struct addrinfo;

namespace yukinal::ssh_service::detail {

using GetAddrInfo = std::function<int(const char*, const char*, const addrinfo*, addrinfo**)>;

enum class AddressResolution {
  resolved,
  failed,
  timed_out,
  busy,
  unavailable,
};

AddressResolution resolve_address_list(
    const std::string& host, const std::string& service, const addrinfo& hints,
    std::chrono::steady_clock::time_point deadline, addrinfo** addresses,
    GetAddrInfo resolver = {});

}  // namespace yukinal::ssh_service::detail

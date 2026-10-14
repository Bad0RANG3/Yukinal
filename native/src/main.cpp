#include <cstdlib>
#include <charconv>
#include <cstdint>
#include <exception>
#include <iostream>
#include <optional>
#include <string>

#include "yukinal/config.hpp"
#include "yukinal/http_service.hpp"

namespace {

void print_usage() {
  std::cerr << "Usage: yukinal-ssh-service --config <path> [--parent-pid <pid>]\n";
}

bool valid_token(const std::string& token) {
  if (token.size() < 32 || token.size() > 256) return false;
  for (const unsigned char ch : token) {
    if (!((ch >= 'A' && ch <= 'Z') || (ch >= 'a' && ch <= 'z') ||
          (ch >= '0' && ch <= '9') || ch == '.' || ch == '_' || ch == '~' || ch == '-')) {
      return false;
    }
  }
  return true;
}

}  // namespace

int main(int argc, char** argv) {
  std::string config_path;
  std::optional<std::uint64_t> parent_pid;
  for (int i = 1; i < argc; ++i) {
    const std::string argument = argv[i];
    if (argument == "--config" && i + 1 < argc) {
      config_path = argv[++i];
    } else if (argument == "--parent-pid" && i + 1 < argc) {
      const std::string value = argv[++i];
      std::uint64_t parsed = 0;
      const auto result = std::from_chars(value.data(), value.data() + value.size(), parsed);
      if (result.ec != std::errc{} || result.ptr != value.data() + value.size() || parsed < 2) {
        print_usage();
        return 2;
      }
      parent_pid = parsed;
    } else if (argument == "--help") {
      print_usage();
      return 0;
    } else {
      print_usage();
      return 2;
    }
  }
  if (config_path.empty()) {
    print_usage();
    return 2;
  }

  try {
    const auto config = yukinal::ssh_service::load_config(config_path);
    const char* token_value = std::getenv(config.api_token_env.c_str());
    if (!token_value || !valid_token(token_value)) {
      std::cerr << "SSH service startup failed: configured API token is missing or invalid\n";
      return 2;
    }
    std::cerr << "SSH service listening on 127.0.0.1:" << config.listen_port
              << " with " << config.targets.size() << " configured target(s)\n";
    if (!yukinal::ssh_service::serve(config, token_value,
                                     yukinal::ssh_service::service_status, parent_pid)) {
      std::cerr << "SSH service failed to bind its loopback HTTP socket\n";
      return 1;
    }
  } catch (const std::exception& error) {
    std::cerr << "SSH service startup failed: " << error.what() << '\n';
    return 2;
  }
  return 0;
}

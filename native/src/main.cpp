#include <csignal>
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
  std::cerr << "Usage: yukinal-ssh-service --config <path> [--parent-pid <pid>] [--terminal --cols <n> --rows <n> | --sftp-download --path <path> | --sftp-upload-exclusive --path <path>]\n";
}

bool parse_dimension(const std::string& value, std::uint16_t* output) {
  unsigned int parsed = 0;
  const auto result = std::from_chars(value.data(), value.data() + value.size(), parsed);
  if (result.ec != std::errc{} || result.ptr != value.data() + value.size() ||
      parsed == 0 || parsed > 1000) {
    return false;
  }
  *output = static_cast<std::uint16_t>(parsed);
  return true;
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
#ifndef _WIN32
  // A peer reset (RST) on a socket that still has buffered output delivers
  // SIGPIPE, whose default disposition terminates the process without running
  // cleanup. Every user of this binary owns a long-lived local service or
  // worker -- the HTTP listener, or a `--terminal` / `--sftp-*` worker on
  // stdin/stdout -- so one reset remote host would otherwise kill the whole
  // process and, for the HTTP listener, every in-flight operation. Ignoring
  // SIGPIPE turns those writes into an ordinary `EPIPE`/`ECONNRESET` error
  // return, which the existing socket and TLS error paths already surface.
  // Installed before any socket or OpenSSL state is created. Windows has no
  // SIGPIPE concept (writes fail with WSAECONNRESET instead), so the call is
  // compiled out there; `SO_NOSIGPIPE`/`MSG_NOSIGNAL` in ssh.cpp only cover
  // the direct ssh.cpp socket sends, not OpenSSL's own TLS writes.
  std::signal(SIGPIPE, SIG_IGN);
#endif

  std::string config_path;
  std::optional<std::uint64_t> parent_pid;
  bool terminal_mode = false;
  bool sftp_download_mode = false;
  bool sftp_upload_mode = false;
  std::string sftp_path;
  std::uint16_t columns = 80;
  std::uint16_t rows = 24;
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
    } else if (argument == "--terminal") {
      terminal_mode = true;
    } else if (argument == "--sftp-download") {
      sftp_download_mode = true;
    } else if (argument == "--sftp-upload-exclusive") {
      sftp_upload_mode = true;
    } else if (argument == "--path" && i + 1 < argc) {
      sftp_path = argv[++i];
    } else if (argument == "--cols" && i + 1 < argc) {
      if (!parse_dimension(argv[++i], &columns)) {
        print_usage();
        return 2;
      }
    } else if (argument == "--rows" && i + 1 < argc) {
      if (!parse_dimension(argv[++i], &rows)) {
        print_usage();
        return 2;
      }
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
    const int worker_modes = static_cast<int>(terminal_mode) +
                             static_cast<int>(sftp_download_mode) +
                             static_cast<int>(sftp_upload_mode);
    if (worker_modes > 0) {
      if (worker_modes != 1 ||
          ((sftp_download_mode || sftp_upload_mode) && sftp_path.empty())) {
        print_usage();
        return 2;
      }
      if (config.targets.size() != 1) {
        std::cerr << "SSH worker startup failed: exactly one target is required\n";
        return 2;
      }
      const auto& target = config.targets.begin()->second;
      if (terminal_mode) {
        return yukinal::ssh_service::run_terminal_session(target, columns, rows);
      }
      return yukinal::ssh_service::run_sftp_stream(target, sftp_path, sftp_upload_mode);
    }
    const char* token_value = std::getenv(config.api_token_env.c_str());
    if (!token_value || !valid_token(token_value)) {
      std::cerr << "SSH service startup failed: configured API token is missing or invalid\n";
      return 2;
    }
    std::cerr << "SSH service listening on 127.0.0.1:" << config.listen_port
              << " with " << config.targets.size() << " configured target(s)\n";
    if (!yukinal::ssh_service::serve(config, token_value,
                                     yukinal::ssh_service::run_operation, parent_pid)) {
      std::cerr << "SSH service failed to bind its loopback HTTP socket\n";
      return 1;
    }
  } catch (const std::exception& error) {
    std::cerr << "SSH service startup failed: " << error.what() << '\n';
    return 2;
  }
  return 0;
}

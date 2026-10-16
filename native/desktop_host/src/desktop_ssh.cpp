#include "yukinal/desktop_host/desktop_ssh.hpp"

#include "yukinal/desktop_host/credential_store.hpp"
#include "yukinal/desktop_host/database.hpp"
#include "yukinal/desktop_host/desktop_target.hpp"

#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
#include <cstdlib>

#include "yukinal/config.hpp"
#include "yukinal/ssh.hpp"
#endif

namespace yukinal::desktop_host {
namespace {

#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
// The C++ SSH core resolves password/key/passphrase/proxy secrets from the
// process environment by name. The desktop IPC runs one command at a time on the
// WebView thread, so a scoped override is safe; the Python sidecar is a separate
// process and never shares this block.
class ScopedEnvironment {
 public:
  explicit ScopedEnvironment(
      const std::vector<std::pair<std::string, std::string>>& values) {
    for (const auto& [name, value] : values) {
      const char* previous = std::getenv(name.c_str());
      saved_.emplace_back(name, previous == nullptr ? std::nullopt
                                                    : std::optional<std::string>(previous));
#ifdef _WIN32
      _putenv_s(name.c_str(), value.c_str());
#else
      setenv(name.c_str(), value.c_str(), 1);
#endif
    }
  }
  ~ScopedEnvironment() {
    for (const auto& [name, previous] : saved_) {
#ifdef _WIN32
      _putenv_s(name.c_str(), previous.has_value() ? previous->c_str() : "");
#else
      if (previous.has_value()) {
        setenv(name.c_str(), previous->c_str(), 1);
      } else {
        unsetenv(name.c_str());
      }
#endif
    }
  }
  ScopedEnvironment(const ScopedEnvironment&) = delete;
  ScopedEnvironment& operator=(const ScopedEnvironment&) = delete;

 private:
  std::vector<std::pair<std::string, std::optional<std::string>>> saved_;
};

yukinal::ssh_service::TargetConfig build_target_config(const nlohmann::json& payload) {
  const auto& target = payload.at("target");
  const auto target_id = target.at("target_id").get<std::string>();
  const nlohmann::json document = {
      {"listen_port", 8765},
      {"api_token_env", "YUKINAL_SSH_API_TOKEN"},
      {"targets", nlohmann::json::array({target})},
  };
  auto config = yukinal::ssh_service::parse_config(document);
  return config.targets.at(target_id);
}

std::vector<std::pair<std::string, std::string>> credential_values(
    const nlohmann::json& payload) {
  std::vector<std::pair<std::string, std::string>> values;
  for (auto item = payload.at("credentials").begin(); item != payload.at("credentials").end();
       ++item) {
    values.emplace_back(item.key(), item.value().get<std::string>());
  }
  return values;
}

SshCommandResult to_result(const yukinal::ssh_service::ExecResult& result) {
  SshCommandResult converted;
  converted.completed = result.completed;
  converted.exit_code = result.exit_code;
  converted.stdout_text = result.stdout_text;
  converted.stderr_text = result.stderr_text;
  converted.error_code = result.error_code;
  converted.error_message = result.error_message;
  return converted;
}
#endif

}  // namespace

SshCommandResult run_server_logs(const DesktopDatabase& database,
                                 const CredentialStore& credentials,
                                 const std::string& server_id,
                                 std::optional<std::uint32_t> since_seconds,
                                 const std::string& unit, int timeout_seconds) {
#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
  if (timeout_seconds < 1 || timeout_seconds > 30) {
    throw std::runtime_error("the server.logs timeout is out of range");
  }
  const auto payload = resolve_ssh_service_target(database, credentials, server_id);
  const auto target = build_target_config(payload);
  ScopedEnvironment environment(credential_values(payload));
  return to_result(
      yukinal::ssh_service::server_logs(target, since_seconds, unit, timeout_seconds));
#else
  (void)database;
  (void)credentials;
  (void)server_id;
  (void)since_seconds;
  (void)unit;
  (void)timeout_seconds;
  throw std::runtime_error("the C++ SSH core is not available in this build");
#endif
}

SshCommandResult run_host_key_probe(const DesktopDatabase& database,
                                    const CredentialStore& credentials,
                                    const std::string& server_id, int timeout_seconds) {
#ifdef YUKINAL_DESKTOP_HOST_WITH_SSH_CORE
  if (timeout_seconds < 1 || timeout_seconds > 30) {
    throw std::runtime_error("the host-key probe timeout is out of range");
  }
  const auto payload = resolve_ssh_service_probe_target(database, credentials, server_id);
  const auto target = build_target_config(payload);
  ScopedEnvironment environment(credential_values(payload));
  return to_result(yukinal::ssh_service::host_key_probe(target, timeout_seconds));
#else
  (void)database;
  (void)credentials;
  (void)server_id;
  (void)timeout_seconds;
  throw std::runtime_error("the C++ SSH core is not available in this build");
#endif
}

}  // namespace yukinal::desktop_host

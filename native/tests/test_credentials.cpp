#include "test_support.hpp"
#include "ssh_credentials.hpp"
#include "yukinal/ssh.hpp"

#include <array>
#include <atomic>
#include <barrier>
#include <cstdlib>
#include <optional>
#include <stdexcept>
#include <thread>

#ifdef _WIN32
#ifndef NOMINMAX
#define NOMINMAX
#endif
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>
#endif

namespace {

using yukinal::ssh_service::ScopedCredentialOverride;
using yukinal::ssh_service::detail::read_ssh_credential;
constexpr std::array<const char*, 4> kNames{
    "YUKINAL_SSH_OVERRIDE_TEST_PASSWORD", "YUKINAL_SSH_OVERRIDE_TEST_PRIVATE_KEY",
    "YUKINAL_SSH_OVERRIDE_TEST_PASSPHRASE", "YUKINAL_SSH_OVERRIDE_TEST_PROXY"};
constexpr const char* kProcessValue = "local-test-process-baseline";

// Only private dummy test variables are read/changed. No desktop store, target
// configuration, actual SSH credential, or credential-bearing argv is used.
class TestProcessCredentials {
 public:
  TestProcessCredentials() {
    for (const auto* name : kNames) {
      const char* old = std::getenv(name);
      saved_.emplace_back(name, old == nullptr ? std::nullopt : std::optional<std::string>(old));
      set(name, kProcessValue);
    }
  }
  ~TestProcessCredentials() {
    for (const auto& [name, value] : saved_) set(name.c_str(), value ? value->c_str() : nullptr);
  }
 private:
  static void set(const char* name, const char* value) {
#ifdef _WIN32
    _putenv_s(name, value == nullptr ? "" : value);
#else
    if (value == nullptr) unsetenv(name); else setenv(name, value, 1);
#endif
  }
  std::vector<std::pair<std::string, std::optional<std::string>>> saved_;
};

ScopedCredentialOverride::Values values(const std::string& label) {
  ScopedCredentialOverride::Values result;
  for (const auto* name : kNames) result.emplace_back(name, label + ":" + name);
  return result;
}

bool matches(const std::string& label) {
  for (const auto* name : kNames) {
    if (read_ssh_credential(name) != label + ":" + name) return false;
    const char* process = std::getenv(name);
    if (process == nullptr || process != std::string(kProcessValue)) return false;
#ifdef _WIN32
    std::array<char, 128> inherited{};
    const auto length = GetEnvironmentVariableA(name, inherited.data(), static_cast<DWORD>(inherited.size()));
    if (length == 0 || inherited.data() != std::string(kProcessValue)) return false;
#endif
  }
  return true;
}

}  // namespace

YUKINAL_TEST(ssh_credential_override_preserves_cli_fallback_and_process_environment) {
  TestProcessCredentials process;
  for (const auto* name : kNames) YUKINAL_REQUIRE(read_ssh_credential(name) == kProcessValue);
  {
    ScopedCredentialOverride operation(values("operation"));
    YUKINAL_REQUIRE(matches("operation"));
    YUKINAL_REQUIRE(read_ssh_credential("").empty());
  }
  for (const auto* name : kNames) YUKINAL_REQUIRE(read_ssh_credential(name) == kProcessValue);
}

YUKINAL_TEST(ssh_credential_override_restores_nested_scope_after_exception) {
  TestProcessCredentials process;
  {
    ScopedCredentialOverride outer(values("outer"));
    YUKINAL_REQUIRE(matches("outer"));
    bool inner_matched = false;
    try {
      ScopedCredentialOverride inner(values("inner"));
      inner_matched = matches("inner");
      throw std::runtime_error("local test operation failed");
    } catch (const std::runtime_error&) {}
    YUKINAL_REQUIRE(inner_matched);
    YUKINAL_REQUIRE(matches("outer"));
    {
      ScopedCredentialOverride empty({});
      for (const auto* name : kNames) YUKINAL_REQUIRE(read_ssh_credential(name).empty());
    }
    YUKINAL_REQUIRE(matches("outer"));
    {
      ScopedCredentialOverride partial({{kNames[0], "partial-password"}});
      YUKINAL_REQUIRE(read_ssh_credential(kNames[0]) == "partial-password");
      for (std::size_t index = 1; index < kNames.size(); ++index) {
        YUKINAL_REQUIRE(read_ssh_credential(kNames[index]).empty());
      }
    }
    YUKINAL_REQUIRE(matches("outer"));
  }
  for (const auto* name : kNames) YUKINAL_REQUIRE(read_ssh_credential(name) == kProcessValue);
}

YUKINAL_TEST(ssh_credential_overrides_are_isolated_across_concurrent_operations) {
  TestProcessCredentials process;
  std::barrier rendezvous(3);
  std::atomic_bool isolated{true};
  const auto work = [&](std::string label) {
    {
      ScopedCredentialOverride operation(values(label));
      rendezvous.arrive_and_wait();
      for (int iteration = 0; iteration < 2000; ++iteration) {
        if (!matches(label)) isolated.store(false);
        if (iteration % 50 == 0) std::this_thread::yield();
      }
      rendezvous.arrive_and_wait();
    }
    for (const auto* name : kNames) {
      if (read_ssh_credential(name) != kProcessValue) isolated.store(false);
    }
  };
  std::thread first(work, "target-A"), second(work, "target-B");
  rendezvous.arrive_and_wait();
  // An unrelated thread cannot see either operation's password/key/proxy data.
  for (const auto* name : kNames) {
    if (read_ssh_credential(name) != kProcessValue) isolated.store(false);
  }
  rendezvous.arrive_and_wait();
  first.join();
  second.join();
  YUKINAL_REQUIRE(isolated.load());
}

#ifdef _WIN32
YUKINAL_TEST(ssh_credential_override_is_not_inherited_by_a_new_process) {
  TestProcessCredentials process;
  ScopedCredentialOverride operation(values("host-operation"));
  YUKINAL_REQUIRE(matches("host-operation"));
  std::array<wchar_t, 32768> executable{};
  const auto length = GetModuleFileNameW(nullptr, executable.data(), static_cast<DWORD>(executable.size()));
  YUKINAL_REQUIRE(length != 0 && length < executable.size());
  std::wstring command = L"\"" + std::wstring(executable.data()) + L"\" --credential-inheritance-check";
  STARTUPINFOW startup{};
  startup.cb = sizeof(startup);
  PROCESS_INFORMATION child{};
  YUKINAL_REQUIRE(CreateProcessW(executable.data(), command.data(), nullptr, nullptr, FALSE,
                                  CREATE_NO_WINDOW, nullptr, nullptr, &startup, &child) != FALSE);
  const auto waited = WaitForSingleObject(child.hProcess, 5000);
  DWORD exit_code = 1;
  (void)GetExitCodeProcess(child.hProcess, &exit_code);
  if (waited == WAIT_TIMEOUT) {
    TerminateProcess(child.hProcess, 1);
    WaitForSingleObject(child.hProcess, 1000);
  }
  CloseHandle(child.hThread);
  CloseHandle(child.hProcess);
  YUKINAL_REQUIRE(waited == WAIT_OBJECT_0 && exit_code == 0);
  YUKINAL_REQUIRE(matches("host-operation"));
}
#endif

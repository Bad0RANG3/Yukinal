#pragma once

#include <cstdlib>
#include <mutex>
#include <optional>
#include <string>
#include <utility>
#include <vector>

namespace yukinal::desktop_host {

// A process-wide credential window for the C++ SSH core. The SSH core resolves
// password/key/passphrase/proxy secrets from the process environment by name,
// but host RPCs run on a sidecar worker pool and transfers run on detached
// worker threads, so two operations can overlap. One process-wide mutex is held
// for the entire lifetime of the scope, serializing every credential-window
// user, and the prior values are restored in reverse order.
class ScopedEnvironment {
 public:
  explicit ScopedEnvironment(
      const std::vector<std::pair<std::string, std::string>>& values)
      : lock_(environment_mutex()) {
    saved_.reserve(values.size());
    try {
      for (const auto& [name, value] : values) {
        const char* previous = std::getenv(name.c_str());
        saved_.emplace_back(
            name, previous == nullptr ? std::nullopt
                                      : std::optional<std::string>(previous));
#ifdef _WIN32
        (void)_putenv_s(name.c_str(), value.c_str());
#else
        (void)setenv(name.c_str(), value.c_str(), 1);
#endif
      }
    } catch (...) {
      restore();
      throw;
    }
  }

  ~ScopedEnvironment() { restore(); }

  ScopedEnvironment(const ScopedEnvironment&) = delete;
  ScopedEnvironment& operator=(const ScopedEnvironment&) = delete;

 private:
  static std::mutex& environment_mutex() {
    static std::mutex mutex;
    return mutex;
  }

  void restore() noexcept {
    for (auto entry = saved_.rbegin(); entry != saved_.rend(); ++entry) {
#ifdef _WIN32
      (void)_putenv_s(entry->first.c_str(),
                      entry->second.has_value() ? entry->second->c_str() : "");
#else
      if (entry->second.has_value()) {
        (void)setenv(entry->first.c_str(), entry->second->c_str(), 1);
      } else {
        (void)unsetenv(entry->first.c_str());
      }
#endif
    }
  }

  // Declared before the saved values so the mutex stays locked while the
  // destructor restores the previous process environment.
  std::unique_lock<std::mutex> lock_;
  std::vector<std::pair<std::string, std::optional<std::string>>> saved_;
};

}  // namespace yukinal::desktop_host
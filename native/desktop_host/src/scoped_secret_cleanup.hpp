#pragma once

#include <nlohmann/json.hpp>
#include <string>
#include <vector>

namespace yukinal::desktop_host {

template <typename Character>
void wipe_secret(std::basic_string<Character>& value) noexcept {
  volatile Character* bytes = value.empty() ? nullptr : value.data();
  for (std::size_t index = 0; index < value.size(); ++index) bytes[index] = 0;
  value.clear();
}

template <typename Value>
void wipe_secret(std::vector<Value>& values) noexcept {
  for (auto& value : values) wipe_secret(value);
  values.clear();
}

inline void wipe_secret(nlohmann::json& value) noexcept {
  if (value.is_string()) wipe_secret(value.get_ref<nlohmann::json::string_t&>());
  else if (value.is_array() || value.is_object()) {
    for (auto& child : value) wipe_secret(child);
  }
  value.clear();
}

/** Owns no copy: the referenced buffer must outlive this guard. */
template <typename Value>
class ScopedSecretCleanup {
 public:
  explicit ScopedSecretCleanup(Value& value) noexcept : value_(value) {}
  ~ScopedSecretCleanup() { clear(); }
  ScopedSecretCleanup(const ScopedSecretCleanup&) = delete;
  ScopedSecretCleanup& operator=(const ScopedSecretCleanup&) = delete;
  void clear() noexcept { wipe_secret(value_); }

 private:
  Value& value_;
};

}  // namespace yukinal::desktop_host

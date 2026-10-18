#include "yukinal/ssh.hpp"
#include "ssh_credentials.hpp"

#include <cstdlib>
#include <utility>

namespace yukinal::ssh_service {
namespace {

thread_local const ScopedCredentialOverride::Values* current_credentials = nullptr;

void erase_value(std::string& value) noexcept {
  volatile char* data = value.empty() ? nullptr : value.data();
  for (std::size_t index = 0; index < value.size(); ++index) data[index] = 0;
  value.clear();
}

}  // namespace

ScopedCredentialOverride::ScopedCredentialOverride(Values values)
    : values_(std::move(values)), previous_(current_credentials) {
  current_credentials = &values_;
}

ScopedCredentialOverride::~ScopedCredentialOverride() {
  current_credentials = previous_;
  for (auto& [name, value] : values_) erase_value(value);
}

namespace detail {

std::string read_ssh_credential(const std::string& name) {
  if (name.empty()) return {};
  if (current_credentials != nullptr) {
    for (auto item = current_credentials->rbegin(); item != current_credentials->rend(); ++item) {
      if (item->first == name) return item->second;
    }
    // A host operation must never pick up another scope or process credential.
    return {};
  }
  const char* value = std::getenv(name.c_str());
  return value == nullptr ? std::string{} : std::string(value);
}

}  // namespace detail
}  // namespace yukinal::ssh_service

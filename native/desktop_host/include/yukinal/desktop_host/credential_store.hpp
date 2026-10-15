#pragma once

#include <cstddef>
#include <cstdint>
#include <stdexcept>
#include <string>
#include <string_view>
#include <vector>

namespace yukinal::desktop_host {

class Secret {
 public:
  explicit Secret(std::vector<std::uint8_t> bytes);
  explicit Secret(std::string_view utf8);
  ~Secret();
  Secret(const Secret&) = delete;
  Secret& operator=(const Secret&) = delete;
  Secret(Secret&& other) noexcept;
  Secret& operator=(Secret&& other) noexcept;

  [[nodiscard]] const std::uint8_t* data() const noexcept { return bytes_.data(); }
  [[nodiscard]] std::size_t size() const noexcept { return bytes_.size(); }
  [[nodiscard]] const std::vector<std::uint8_t>& bytes() const noexcept { return bytes_; }

 private:
  void clear() noexcept;
  std::vector<std::uint8_t> bytes_;
};

struct CredentialReference {
  std::string service;
  std::string account;

  [[nodiscard]] static CredentialReference parse(std::string_view value);
  [[nodiscard]] std::string to_string() const;
};

// The credential boundary. The desktop host only ever talks to this interface so
// commands can be tested without touching the operating system credential store.
class CredentialStore {
 public:
  virtual ~CredentialStore() = default;
  [[nodiscard]] virtual CredentialReference set(const std::string& service,
                                                const std::string& account,
                                                const Secret& secret) const = 0;
  [[nodiscard]] virtual Secret get(const CredentialReference& reference) const = 0;
  [[nodiscard]] virtual bool has(const CredentialReference& reference) const = 0;
  virtual void erase(const CredentialReference& reference) const = 0;
};

class DesktopCredentialStore : public CredentialStore {
 public:
  [[nodiscard]] CredentialReference set(const std::string& service,
                                        const std::string& account,
                                        const Secret& secret) const override;
  [[nodiscard]] Secret get(const CredentialReference& reference) const override;
  [[nodiscard]] bool has(const CredentialReference& reference) const override;
  void erase(const CredentialReference& reference) const override;
};

class CredentialStoreError : public std::runtime_error {
 public:
  using std::runtime_error::runtime_error;
};

class CredentialNotFoundError : public CredentialStoreError {
 public:
  using CredentialStoreError::CredentialStoreError;
};

}  // namespace yukinal::desktop_host

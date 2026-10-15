#pragma once

#include <cstdint>
#include <filesystem>
#include <map>
#include <optional>
#include <string>

namespace yukinal::desktop_host {

// ADR 0012: pins are confirmed by an explicit user action and stored in a narrow
// versioned file (`v1:host:port:SHA256:...`). Changed keys need two actions
// (forget, then probe-and-confirm); there is no "accept anyway" path.
enum class HostKeyComparison { Unpinned, Matches, Mismatch };

struct HostKeyCheck {
  HostKeyComparison comparison = HostKeyComparison::Unpinned;
  std::optional<std::string> pinned;
};

enum class HostKeyForgetOutcome { Removed, NothingPinned };

class HostTrustStore {
 public:
  // A missing file is an empty store; a malformed file is an error.
  [[nodiscard]] static HostTrustStore load(const std::filesystem::path& path);

  [[nodiscard]] std::optional<std::string> pinned_fingerprint(const std::string& host,
                                                              std::uint16_t port) const;
  [[nodiscard]] HostKeyCheck check(const std::string& host, std::uint16_t port,
                                   const std::string& presented) const;

  // Writes the confirmed fingerprint and persists when a path is configured.
  void pin(const std::string& host, std::uint16_t port, const std::string& fingerprint);

  // Removes a pin and persists. Returns what actually happened.
  [[nodiscard]] HostKeyForgetOutcome forget(const std::string& host, std::uint16_t port);

  [[nodiscard]] bool empty() const noexcept { return entries_.empty(); }

 private:
  void save() const;

  std::optional<std::filesystem::path> path_;
  std::map<std::pair<std::string, std::uint16_t>, std::string> entries_;
};

}  // namespace yukinal::desktop_host

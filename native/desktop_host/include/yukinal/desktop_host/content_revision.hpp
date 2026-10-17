#pragma once

#include <cstddef>
#include <cstdint>
#include <string>

namespace yukinal::desktop_host {

// Lowercase-hex SHA-256 over the raw bytes of `input`.
//
// This is the single hashing entry point the desktop host uses for content
// revisions (`filesystem.read`/`edit` revisions, evidence content hashes) and for
// the host-derived filesystem backup path. It lives behind a narrow declaration so
// the implementation (a dependency-free SHA-256) stays private to one translation
// unit while every caller agrees on the exact bytes and encoding.
[[nodiscard]] std::string content_revision_hex(const std::string& input);

// Incremental SHA-256 over raw bytes. Use it to hash a stream (a local file, a
// remote read handle) without buffering it whole. `hexdigest()` finalizes and may
// be called only once; for a single in-memory string use `content_revision_hex`.
class ContentHasher {
 public:
  void update(const void* data, std::size_t size);
  [[nodiscard]] std::string hexdigest();

 private:
  static std::uint32_t rotr(std::uint32_t value, std::uint32_t bits);
  void transform();

  unsigned char buffer_[64] = {};
  std::size_t buffer_length_ = 0;
  std::uint64_t bit_length_ = 0;
  std::uint32_t digest_[8] = {0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                              0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19};
};

}  // namespace yukinal::desktop_host

#include "yukinal/desktop_host/content_revision.hpp"

#include <cstddef>
#include <cstdint>

namespace yukinal::desktop_host {

// A small, dependency-free SHA-256 (FIPS 180-4). The desktop host must compute the
// content hash the same way every other Yukinal component does, and linking a
// platform crypto library here would duplicate what `host_key.cpp` does per platform.
void ContentHasher::update(const void* data, std::size_t length) {
  const auto* bytes = static_cast<const unsigned char*>(data);
  for (std::size_t i = 0; i < length; ++i) {
    buffer_[buffer_length_++] = bytes[i];
    if (buffer_length_ == 64) {
      transform();
      bit_length_ += 512;
      buffer_length_ = 0;
    }
  }
}

std::uint32_t ContentHasher::rotr(std::uint32_t value, std::uint32_t bits) {
  return (value >> bits) | (value << (32 - bits));
}

void ContentHasher::transform() {
  static const std::uint32_t k[64] = {
      0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
      0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
      0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
      0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
      0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
      0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
      0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
      0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
      0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
      0xc67178f2};
  std::uint32_t w[64];
  for (int i = 0; i < 16; ++i) {
    w[i] = (static_cast<std::uint32_t>(buffer_[i * 4]) << 24) |
           (static_cast<std::uint32_t>(buffer_[i * 4 + 1]) << 16) |
           (static_cast<std::uint32_t>(buffer_[i * 4 + 2]) << 8) |
           static_cast<std::uint32_t>(buffer_[i * 4 + 3]);
  }
  for (int i = 16; i < 64; ++i) {
    const std::uint32_t s0 = rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ (w[i - 15] >> 3);
    const std::uint32_t s1 = rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ (w[i - 2] >> 10);
    w[i] = w[i - 16] + s0 + w[i - 7] + s1;
  }
  std::uint32_t a = digest_[0], b = digest_[1], c = digest_[2], d = digest_[3];
  std::uint32_t e = digest_[4], f = digest_[5], g = digest_[6], h = digest_[7];
  for (int i = 0; i < 64; ++i) {
    const std::uint32_t s1 = rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25);
    const std::uint32_t ch = (e & f) ^ (~e & g);
    const std::uint32_t temp1 = h + s1 + ch + k[i] + w[i];
    const std::uint32_t s0 = rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22);
    const std::uint32_t maj = (a & b) ^ (a & c) ^ (b & c);
    const std::uint32_t temp2 = s0 + maj;
    h = g; g = f; f = e; e = d + temp1;
    d = c; c = b; b = a; a = temp1 + temp2;
  }
  digest_[0] += a; digest_[1] += b; digest_[2] += c; digest_[3] += d;
  digest_[4] += e; digest_[5] += f; digest_[6] += g; digest_[7] += h;
}

std::string ContentHasher::hexdigest() {
  const std::uint64_t bit_length = bit_length_ + static_cast<std::uint64_t>(buffer_length_) * 8;
  buffer_[buffer_length_++] = 0x80;
  if (buffer_length_ > 56) {
    while (buffer_length_ < 64) buffer_[buffer_length_++] = 0;
    transform();
    buffer_length_ = 0;
  }
  while (buffer_length_ < 56) buffer_[buffer_length_++] = 0;
  for (int i = 7; i >= 0; --i) {
    buffer_[buffer_length_++] = static_cast<unsigned char>((bit_length >> (i * 8)) & 0xff);
  }
  transform();
  static const char digits[] = "0123456789abcdef";
  std::string out;
  out.reserve(64);
  for (const std::uint32_t word : digest_) {
    for (int shift = 24; shift >= 0; shift -= 8) {
      const auto byte = static_cast<unsigned char>((word >> shift) & 0xff);
      out.push_back(digits[byte >> 4]);
      out.push_back(digits[byte & 0x0f]);
    }
  }
  return out;
}

}  // namespace yukinal::desktop_host

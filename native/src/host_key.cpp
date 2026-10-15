#include "host_key.hpp"

#include <array>
#include <cstdint>
#include <limits>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>
#include <bcrypt.h>
#else
#include <openssl/evp.h>
#endif

namespace yukinal::ssh_service::detail {
namespace {

constexpr std::size_t kMaxHostKeyBlobBytes = 1024 * 1024;
constexpr std::size_t kSha256Bytes = 32;

bool read_string(std::string_view input, std::size_t* offset,
                 std::string_view* value) {
  if (!offset || !value || *offset > input.size() || input.size() - *offset < 4) {
    return false;
  }
  const auto* bytes = reinterpret_cast<const unsigned char*>(input.data() + *offset);
  const std::uint32_t length = (static_cast<std::uint32_t>(bytes[0]) << 24) |
      (static_cast<std::uint32_t>(bytes[1]) << 16) |
      (static_cast<std::uint32_t>(bytes[2]) << 8) |
      static_cast<std::uint32_t>(bytes[3]);
  *offset += 4;
  if (length > input.size() - *offset) return false;
  *value = input.substr(*offset, length);
  *offset += length;
  return true;
}

bool append_string(std::string_view value, std::string* output) {
  if (!output || value.size() > std::numeric_limits<std::uint32_t>::max()) return false;
  const auto length = static_cast<std::uint32_t>(value.size());
  output->push_back(static_cast<char>((length >> 24) & 0xffU));
  output->push_back(static_cast<char>((length >> 16) & 0xffU));
  output->push_back(static_cast<char>((length >> 8) & 0xffU));
  output->push_back(static_cast<char>(length & 0xffU));
  output->append(value);
  return true;
}

bool algorithm_for_name(std::string_view name, HostKeyAlgorithm* algorithm) {
  if (!algorithm) return false;
  if (name == "ssh-rsa") *algorithm = HostKeyAlgorithm::rsa;
  else if (name == "ssh-dss") *algorithm = HostKeyAlgorithm::dss;
  else if (name == "ecdsa-sha2-nistp256") *algorithm = HostKeyAlgorithm::ecdsa_256;
  else if (name == "ecdsa-sha2-nistp384") *algorithm = HostKeyAlgorithm::ecdsa_384;
  else if (name == "ecdsa-sha2-nistp521") *algorithm = HostKeyAlgorithm::ecdsa_521;
  else if (name == "ssh-ed25519") *algorithm = HostKeyAlgorithm::ed25519;
  else return false;
  return true;
}

bool certificate_algorithm(std::string_view name, std::string_view* plain_name,
                           HostKeyAlgorithm* algorithm) {
  constexpr std::string_view suffix = "-cert-v01@openssh.com";
  if (!plain_name || !algorithm || name.size() <= suffix.size() ||
      name.substr(name.size() - suffix.size()) != suffix) {
    return false;
  }
  *plain_name = name.substr(0, name.size() - suffix.size());
  return algorithm_for_name(*plain_name, algorithm);
}

bool sha256(std::string_view input, std::array<unsigned char, kSha256Bytes>* digest) {
  if (!digest) return false;
#ifdef _WIN32
  BCRYPT_ALG_HANDLE algorithm = nullptr;
  BCRYPT_HASH_HANDLE hash = nullptr;
  DWORD object_length = 0;
  DWORD returned = 0;
  std::vector<unsigned char> object;
  bool ok = BCryptOpenAlgorithmProvider(
      &algorithm, BCRYPT_SHA256_ALGORITHM, nullptr, 0) >= 0;
  if (ok) {
    ok = BCryptGetProperty(algorithm, BCRYPT_OBJECT_LENGTH,
        reinterpret_cast<PUCHAR>(&object_length), sizeof(object_length), &returned, 0) >= 0 &&
        object_length > 0;
  }
  if (ok) {
    object.resize(object_length);
    ok = BCryptCreateHash(algorithm, &hash, object.data(), object_length,
                          nullptr, 0, 0) >= 0;
  }
  if (ok && !input.empty()) {
    ok = BCryptHashData(hash,
        const_cast<PUCHAR>(reinterpret_cast<const UCHAR*>(input.data())),
        static_cast<ULONG>(input.size()), 0) >= 0;
  }
  if (ok) {
    ok = BCryptFinishHash(hash, digest->data(), static_cast<ULONG>(digest->size()), 0) >= 0;
  }
  if (hash) BCryptDestroyHash(hash);
  if (algorithm) BCryptCloseAlgorithmProvider(algorithm, 0);
  return ok;
#else
  unsigned int digest_length = 0;
  return EVP_Digest(input.data(), input.size(), digest->data(), &digest_length,
                    EVP_sha256(), nullptr) == 1 && digest_length == digest->size();
#endif
}

std::string base64_unpadded(const std::array<unsigned char, kSha256Bytes>& bytes) {
  constexpr char alphabet[] =
      "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  std::string encoded;
  encoded.reserve(43);
  for (std::size_t i = 0; i < bytes.size(); i += 3) {
    const unsigned int first = bytes[i];
    const unsigned int second = i + 1 < bytes.size() ? bytes[i + 1] : 0;
    const unsigned int third = i + 2 < bytes.size() ? bytes[i + 2] : 0;
    const unsigned int block = (first << 16) | (second << 8) | third;
    encoded.push_back(alphabet[(block >> 18) & 0x3fU]);
    encoded.push_back(alphabet[(block >> 12) & 0x3fU]);
    if (i + 1 < bytes.size()) encoded.push_back(alphabet[(block >> 6) & 0x3fU]);
    if (i + 2 < bytes.size()) encoded.push_back(alphabet[block & 0x3fU]);
  }
  return encoded;
}

}  // namespace

std::optional<HostKeyMaterial> normalize_host_key_blob(std::string_view blob) {
  if (blob.empty() || blob.size() > kMaxHostKeyBlobBytes) return std::nullopt;

  std::size_t offset = 0;
  std::string_view name;
  if (!read_string(blob, &offset, &name) || name.empty()) return std::nullopt;

  HostKeyAlgorithm algorithm{};
  std::string_view plain_name;
  if (!certificate_algorithm(name, &plain_name, &algorithm)) {
    if (!algorithm_for_name(name, &algorithm)) return std::nullopt;
    return HostKeyMaterial{std::string(blob), algorithm, false};
  }

  std::string_view nonce;
  if (!read_string(blob, &offset, &nonce) || nonce.size() < 16 || nonce.size() > 1024) {
    return std::nullopt;
  }

  const std::size_t fields_start = offset;
  std::string_view first_field;
  std::string_view second_field;
  switch (algorithm) {
    case HostKeyAlgorithm::rsa:
      if (!read_string(blob, &offset, &first_field) || first_field.empty() ||
          !read_string(blob, &offset, &second_field) || second_field.empty()) {
        return std::nullopt;
      }
      break;
    case HostKeyAlgorithm::ecdsa_256:
    case HostKeyAlgorithm::ecdsa_384:
    case HostKeyAlgorithm::ecdsa_521: {
      const std::string_view expected_curve = algorithm == HostKeyAlgorithm::ecdsa_256
          ? "nistp256"
          : algorithm == HostKeyAlgorithm::ecdsa_384 ? "nistp384" : "nistp521";
      std::string_view curve;
      if (!read_string(blob, &offset, &curve) || curve != expected_curve ||
          !read_string(blob, &offset, &first_field) || first_field.empty()) {
        return std::nullopt;
      }
      break;
    }
    case HostKeyAlgorithm::ed25519:
      if (!read_string(blob, &offset, &first_field) || first_field.size() != 32) {
        return std::nullopt;
      }
      break;
    case HostKeyAlgorithm::dss:
      return std::nullopt;
  }

  std::string public_key_blob;
  if (!append_string(plain_name, &public_key_blob)) return std::nullopt;
  public_key_blob.append(blob.substr(fields_start, offset - fields_start));
  return HostKeyMaterial{std::move(public_key_blob), algorithm, true};
}

std::string fingerprint_host_key_blob(std::string_view blob) {
  const auto material = normalize_host_key_blob(blob);
  if (!material) return {};
  std::array<unsigned char, kSha256Bytes> digest{};
  if (!sha256(material->public_key_blob, &digest)) return {};
  return "SHA256:" + base64_unpadded(digest);
}

}  // namespace yukinal::ssh_service::detail

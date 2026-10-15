#include "host_key.hpp"
#include "yukinal/config.hpp"

#include <algorithm>
#include <array>
#include <charconv>
#include <chrono>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <limits>
#include <memory>
#include <stdexcept>
#include <string>
#include <string_view>
#include <unordered_set>
#include <vector>

#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <winsock2.h>
#include <ws2tcpip.h>
#else
#include <arpa/inet.h>
#include <netdb.h>
#include <sys/socket.h>
#endif

#include <httplib.h>

#include <openssl/bn.h>
#include <openssl/ec.h>
#include <openssl/ecdsa.h>
#include <openssl/evp.h>
#include <openssl/rsa.h>
#include <openssl/x509v3.h>

#include "ssh_network.hpp"

namespace yukinal::ssh_service::detail {
namespace {

constexpr std::size_t kMaxCertificateBytes = 1024 * 1024;
constexpr std::size_t kMaxKrlBytes = 16 * 1024 * 1024;
constexpr std::string_view kCertificateSuffix = "-cert-v01@openssh.com";
constexpr std::string_view kKrlMagic("SSHKRL\n\0", 8);

bool read_string(std::string_view input, std::size_t* offset, std::string_view* value) {
  if (!offset || !value || *offset > input.size() || input.size() - *offset < 4) return false;
  const auto* bytes = reinterpret_cast<const unsigned char*>(input.data() + *offset);
  const std::uint32_t length = (static_cast<std::uint32_t>(bytes[0]) << 24) |
      (static_cast<std::uint32_t>(bytes[1]) << 16) |
      (static_cast<std::uint32_t>(bytes[2]) << 8) | bytes[3];
  *offset += 4;
  if (length > input.size() - *offset) return false;
  *value = input.substr(*offset, length);
  *offset += length;
  return true;
}

bool append_string(std::string_view value, std::string* output) {
  if (!output || value.size() > UINT32_MAX) return false;
  const auto length = static_cast<std::uint32_t>(value.size());
  output->push_back(static_cast<char>(length >> 24));
  output->push_back(static_cast<char>(length >> 16));
  output->push_back(static_cast<char>(length >> 8));
  output->push_back(static_cast<char>(length));
  output->append(value);
  return true;
}

bool read_u32(std::string_view input, std::size_t* offset, std::uint32_t* value) {
  if (!offset || !value || *offset > input.size() || input.size() - *offset < 4) return false;
  const auto* bytes = reinterpret_cast<const unsigned char*>(input.data() + *offset);
  *value = (static_cast<std::uint32_t>(bytes[0]) << 24) |
      (static_cast<std::uint32_t>(bytes[1]) << 16) |
      (static_cast<std::uint32_t>(bytes[2]) << 8) | bytes[3];
  *offset += 4;
  return true;
}

bool read_u64(std::string_view input, std::size_t* offset, std::uint64_t* value) {
  if (!offset || !value || *offset > input.size() || input.size() - *offset < 8) return false;
  const auto* bytes = reinterpret_cast<const unsigned char*>(input.data() + *offset);
  *value = 0;
  for (int index = 0; index < 8; ++index) *value = (*value << 8) | bytes[index];
  *offset += 8;
  return true;
}

bool decode_base64(std::string_view input, std::string* output) {
  if (!output || input.empty() || input.size() > 16 * 1024 || input.size() % 4 != 0) return false;
  std::size_t padding = 0;
  if (input.ends_with("==")) padding = 2;
  else if (input.ends_with("=")) padding = 1;
  for (std::size_t index = 0; index < input.size() - padding; ++index) {
    const unsigned char ch = static_cast<unsigned char>(input[index]);
    if (!((ch >= 'A' && ch <= 'Z') || (ch >= 'a' && ch <= 'z') ||
          (ch >= '0' && ch <= '9') || ch == '+' || ch == '/')) return false;
  }
  for (std::size_t index = input.size() - padding; index < input.size(); ++index)
    if (input[index] != '=') return false;
  std::string decoded(3 * input.size() / 4, '\0');
  const int length = EVP_DecodeBlock(reinterpret_cast<unsigned char*>(decoded.data()),
      reinterpret_cast<const unsigned char*>(input.data()), static_cast<int>(input.size()));
  if (length < 0 || static_cast<std::size_t>(length) < padding) return false;
  decoded.resize(static_cast<std::size_t>(length) - padding);
  if (decoded.empty() || decoded.size() > kMaxCertificateBytes) return false;
  *output = std::move(decoded);
  return true;
}

using EvpKey = std::unique_ptr<EVP_PKEY, decltype(&EVP_PKEY_free)>;

EvpKey parse_key_blob(std::string_view blob) {
  std::size_t offset = 0;
  std::string_view name;
  if (!read_string(blob, &offset, &name)) return {nullptr, EVP_PKEY_free};
  if (name == "ssh-ed25519") {
    std::string_view key;
    if (!read_string(blob, &offset, &key) || key.size() != 32 || offset != blob.size())
      return {nullptr, EVP_PKEY_free};
    return {EVP_PKEY_new_raw_public_key(EVP_PKEY_ED25519, nullptr,
        reinterpret_cast<const unsigned char*>(key.data()), key.size()), EVP_PKEY_free};
  }
  if (name == "ssh-rsa") {
    std::string_view exponent;
    std::string_view modulus;
    if (!read_string(blob, &offset, &exponent) || exponent.empty() ||
        !read_string(blob, &offset, &modulus) || modulus.empty() || offset != blob.size())
      return {nullptr, EVP_PKEY_free};
    BIGNUM* e = BN_bin2bn(reinterpret_cast<const unsigned char*>(exponent.data()),
                           static_cast<int>(exponent.size()), nullptr);
    BIGNUM* n = BN_bin2bn(reinterpret_cast<const unsigned char*>(modulus.data()),
                           static_cast<int>(modulus.size()), nullptr);
    RSA* rsa = RSA_new();
    if (!e || !n || !rsa || RSA_set0_key(rsa, n, e, nullptr) != 1) {
      BN_free(e); BN_free(n); RSA_free(rsa);
      return {nullptr, EVP_PKEY_free};
    }
    EVP_PKEY* key = EVP_PKEY_new();
    if (!key || EVP_PKEY_assign_RSA(key, rsa) != 1) {
      EVP_PKEY_free(key); RSA_free(rsa);
      return {nullptr, EVP_PKEY_free};
    }
    return {key, EVP_PKEY_free};
  }
  int nid = NID_undef;
  std::string_view expected_curve;
  if (name == "ecdsa-sha2-nistp256") { nid = NID_X9_62_prime256v1; expected_curve = "nistp256"; }
  else if (name == "ecdsa-sha2-nistp384") { nid = NID_secp384r1; expected_curve = "nistp384"; }
  else if (name == "ecdsa-sha2-nistp521") { nid = NID_secp521r1; expected_curve = "nistp521"; }
  else return {nullptr, EVP_PKEY_free};
  std::string_view curve;
  std::string_view point;
  if (!read_string(blob, &offset, &curve) || curve != expected_curve ||
      !read_string(blob, &offset, &point) || point.empty() || offset != blob.size())
    return {nullptr, EVP_PKEY_free};
  EC_KEY* ec = EC_KEY_new_by_curve_name(nid);
  EC_POINT* public_point = ec ? EC_POINT_new(EC_KEY_get0_group(ec)) : nullptr;
  const bool valid = ec && public_point &&
      EC_POINT_oct2point(EC_KEY_get0_group(ec), public_point,
          reinterpret_cast<const unsigned char*>(point.data()), point.size(), nullptr) == 1 &&
      EC_KEY_set_public_key(ec, public_point) == 1 && EC_KEY_check_key(ec) == 1;
  EC_POINT_free(public_point);
  if (!valid) { EC_KEY_free(ec); return {nullptr, EVP_PKEY_free}; }
  EVP_PKEY* key = EVP_PKEY_new();
  if (!key || EVP_PKEY_assign_EC_KEY(key, ec) != 1) {
    EVP_PKEY_free(key); EC_KEY_free(ec);
    return {nullptr, EVP_PKEY_free};
  }
  return {key, EVP_PKEY_free};
}

bool parse_authorized_key(std::string_view line, std::string* blob) {
  if (!blob || line.empty() || line.size() > 16 * 1024) return false;
  const auto start = line.find_first_not_of(" \t\r\n");
  if (start == std::string_view::npos) return false;
  line.remove_prefix(start);
  const auto algorithm_end = line.find_first_of(" \t\r\n");
  if (algorithm_end == std::string_view::npos) return false;
  const auto algorithm = line.substr(0, algorithm_end);
  const auto key_start = line.find_first_not_of(" \t\r\n", algorithm_end);
  if (key_start == std::string_view::npos) return false;
  line.remove_prefix(key_start);
  const auto key_end = line.find_first_of(" \t\r\n");
  if (!decode_base64(line.substr(0, key_end), blob)) return false;
  std::size_t offset = 0;
  std::string_view embedded_algorithm;
  if (!read_string(*blob, &offset, &embedded_algorithm) || embedded_algorithm != algorithm) return false;
  return parse_key_blob(*blob) != nullptr;
}

bool parse_principal_list(std::string_view input, std::vector<std::string>* principals) {
  std::size_t offset = 0;
  while (offset < input.size()) {
    std::string_view item;
    if (!read_string(input, &offset, &item) || item.empty() || item.size() > 253 ||
        principals->size() >= 256) return false;
    principals->emplace_back(item);
  }
  return true;
}

unsigned char ascii_lower(unsigned char value) {
  if (value >= 'A' && value <= 'Z') return static_cast<unsigned char>(value + ('a' - 'A'));
  return value;
}

bool principal_matches(std::string_view pattern, std::string_view value) {
  if (pattern.size() > 253 || value.size() > 253) return false;
  std::vector<unsigned char> previous(value.size() + 1, 0), current(value.size() + 1, 0);
  previous[0] = 1;
  for (const unsigned char token : pattern) {
    std::fill(current.begin(), current.end(), 0);
    if (token == '*') {
      current[0] = previous[0];
      for (std::size_t j = 1; j <= value.size(); ++j)
        current[j] = static_cast<unsigned char>(previous[j] || current[j - 1]);
    } else {
      for (std::size_t j = 1; j <= value.size(); ++j) {
        const auto ch = ascii_lower(static_cast<unsigned char>(value[j - 1]));
        current[j] = static_cast<unsigned char>(previous[j - 1] &&
            (token == '?' || ascii_lower(token) == ch));
      }
    }
    previous.swap(current);
  }
  return previous[value.size()] != 0;
}

bool verify_signature(EVP_PKEY* ca_key, std::string_view ca_blob,
                      std::string_view data, std::string_view signature_blob) {
  std::size_t offset = 0;
  std::string_view algorithm, signature;
  if (!read_string(signature_blob, &offset, &algorithm) ||
      !read_string(signature_blob, &offset, &signature) || offset != signature_blob.size()) return false;
  offset = 0;
  std::string_view key_algorithm;
  if (!read_string(ca_blob, &offset, &key_algorithm)) return false;
  bool ed25519 = false;
  const EVP_MD* digest = nullptr;
  if (algorithm == "ssh-ed25519" && key_algorithm == "ssh-ed25519") ed25519 = true;
  else if (key_algorithm == "ssh-rsa" && algorithm == "ssh-rsa") digest = EVP_sha1();
  else if (key_algorithm == "ssh-rsa" && algorithm == "rsa-sha2-256") digest = EVP_sha256();
  else if (key_algorithm == "ssh-rsa" && algorithm == "rsa-sha2-512") digest = EVP_sha512();
  else if (key_algorithm == "ecdsa-sha2-nistp256" && algorithm == key_algorithm) digest = EVP_sha256();
  else if (key_algorithm == "ecdsa-sha2-nistp384" && algorithm == key_algorithm) digest = EVP_sha384();
  else if (key_algorithm == "ecdsa-sha2-nistp521" && algorithm == key_algorithm) digest = EVP_sha512();
  else return false;
  std::string der_signature;
  if (key_algorithm.rfind("ecdsa-", 0) == 0) {
    std::size_t sig_offset = 0;
    std::string_view r_bytes, s_bytes;
    if (!read_string(signature, &sig_offset, &r_bytes) || r_bytes.empty() ||
        !read_string(signature, &sig_offset, &s_bytes) || s_bytes.empty() ||
        sig_offset != signature.size()) return false;
    BIGNUM* r = BN_bin2bn(reinterpret_cast<const unsigned char*>(r_bytes.data()), static_cast<int>(r_bytes.size()), nullptr);
    BIGNUM* s = BN_bin2bn(reinterpret_cast<const unsigned char*>(s_bytes.data()), static_cast<int>(s_bytes.size()), nullptr);
    ECDSA_SIG* ecdsa = ECDSA_SIG_new();
    if (!r || !s || !ecdsa || ECDSA_SIG_set0(ecdsa, r, s) != 1) {
      BN_free(r); BN_free(s); ECDSA_SIG_free(ecdsa); return false;
    }
    const int length = i2d_ECDSA_SIG(ecdsa, nullptr);
    if (length <= 0) { ECDSA_SIG_free(ecdsa); return false; }
    der_signature.resize(static_cast<std::size_t>(length));
    unsigned char* cursor = reinterpret_cast<unsigned char*>(der_signature.data());
    i2d_ECDSA_SIG(ecdsa, &cursor);
    ECDSA_SIG_free(ecdsa);
    signature = der_signature;
  }
  EVP_MD_CTX* context = EVP_MD_CTX_new();
  if (!context) return false;
  EVP_PKEY_CTX* key_context = nullptr;
  const bool initialized = EVP_DigestVerifyInit(context, &key_context,
      ed25519 ? nullptr : digest, nullptr, ca_key) == 1;
  const bool valid = initialized && EVP_DigestVerify(context,
      reinterpret_cast<const unsigned char*>(signature.data()), signature.size(),
      reinterpret_cast<const unsigned char*>(data.data()), data.size()) == 1;
  EVP_MD_CTX_free(context);
  return valid;
}

bool parse_certificate(std::string_view blob, std::vector<std::string>* principals,
                       std::uint64_t* valid_after, std::uint64_t* valid_before,
                       std::string_view* critical_options, std::string_view* ca_blob,
                       std::string_view* signature_blob, std::size_t* signed_length,
                       std::uint32_t* certificate_type, std::uint64_t* serial_out,
                       std::string_view* key_id_out, std::string* subject_key_blob) {
  std::size_t offset = 0;
  std::string_view algorithm, nonce;
  if (!read_string(blob, &offset, &algorithm) || algorithm.size() <= kCertificateSuffix.size() ||
      algorithm.substr(algorithm.size() - kCertificateSuffix.size()) != kCertificateSuffix ||
      !read_string(blob, &offset, &nonce) || nonce.size() < 16 || nonce.size() > 1024) return false;
  const auto plain = algorithm.substr(0, algorithm.size() - kCertificateSuffix.size());
  const std::size_t subject_fields_start = offset;
  std::string_view field;
  if (plain == "ssh-rsa") {
    if (!read_string(blob, &offset, &field) || field.empty() ||
        !read_string(blob, &offset, &field) || field.empty()) return false;
  } else if (plain == "ssh-ed25519") {
    if (!read_string(blob, &offset, &field) || field.size() != 32) return false;
  } else if (plain == "ecdsa-sha2-nistp256" || plain == "ecdsa-sha2-nistp384" ||
             plain == "ecdsa-sha2-nistp521") {
    const auto expected = plain == "ecdsa-sha2-nistp256" ? "nistp256" :
        plain == "ecdsa-sha2-nistp384" ? "nistp384" : "nistp521";
    std::string_view curve;
    if (!read_string(blob, &offset, &curve) || curve != expected ||
        !read_string(blob, &offset, &field) || field.empty()) return false;
  } else return false;
  if (!append_string(plain, subject_key_blob)) return false;
  subject_key_blob->append(blob.substr(subject_fields_start, offset - subject_fields_start));
  std::string_view principal_blob;
  if (!read_u64(blob, &offset, serial_out) || !read_u32(blob, &offset, certificate_type) ||
      !read_string(blob, &offset, key_id_out) || key_id_out->size() > 4096 ||
      !read_string(blob, &offset, &principal_blob) || !parse_principal_list(principal_blob, principals) ||
      !read_u64(blob, &offset, valid_after) || !read_u64(blob, &offset, valid_before) ||
      !read_string(blob, &offset, critical_options)) return false;
  std::string_view ignored;
  if (!read_string(blob, &offset, &ignored) || !read_string(blob, &offset, &ignored) ||
      !read_string(blob, &offset, ca_blob)) return false;
  *signed_length = offset;
  return read_string(blob, &offset, signature_blob) && offset == blob.size();
}

class KrlReader {
 public:
  explicit KrlReader(std::string_view bytes) : bytes_(bytes) {}
  bool empty() const { return offset_ == bytes_.size(); }
  std::size_t offset() const { return offset_; }
  std::string_view exact(std::size_t length) {
    if (length > bytes_.size() - offset_) throw std::runtime_error("KRL is truncated");
    const auto value = bytes_.substr(offset_, length);
    offset_ += length;
    return value;
  }
  std::uint8_t u8() {
    if (offset_ == bytes_.size()) throw std::runtime_error("KRL is truncated");
    return static_cast<std::uint8_t>(bytes_[offset_++]);
  }
  std::uint32_t u32() {
    std::string_view bytes = exact(4);
    const auto* data = reinterpret_cast<const unsigned char*>(bytes.data());
    return (static_cast<std::uint32_t>(data[0]) << 24) |
        (static_cast<std::uint32_t>(data[1]) << 16) |
        (static_cast<std::uint32_t>(data[2]) << 8) | data[3];
  }
  std::uint64_t u64() {
    std::string_view bytes = exact(8);
    const auto* data = reinterpret_cast<const unsigned char*>(bytes.data());
    std::uint64_t value = 0;
    for (int index = 0; index < 8; ++index) value = (value << 8) | data[index];
    return value;
  }
  std::string_view string() {
    const auto length = static_cast<std::size_t>(u32());
    return exact(length);
  }
  void finish() const {
    if (!empty()) throw std::runtime_error("KRL section contains unparsed data");
  }

 private:
  std::string_view bytes_;
  std::size_t offset_ = 0;
};

struct KrlBitmap {
  std::uint64_t offset = 0;
  std::string bits;
};

struct KrlCertificateSection {
  std::string ca_key;
  std::vector<std::pair<std::uint64_t, std::uint64_t>> serial_ranges;
  std::vector<KrlBitmap> serial_bitmaps;
  std::unordered_set<std::string> key_ids;
};

struct KrlSignature {
  std::size_t signed_length = 0;
  std::string key_blob;
  std::string signature_blob;
};

struct KrlData {
  std::vector<KrlCertificateSection> certificate_sections;
  std::unordered_set<std::string> explicit_keys;
  std::unordered_set<std::string> sha1_fingerprints;
  std::unordered_set<std::string> sha256_fingerprints;
};

bool valid_utf8(std::string_view value) {
  std::size_t index = 0;
  while (index < value.size()) {
    const auto first = static_cast<unsigned char>(value[index++]);
    if (first <= 0x7f) continue;
    std::uint32_t code = 0;
    std::size_t continuation = 0;
    if (first >= 0xc2 && first <= 0xdf) { code = first & 0x1f; continuation = 1; }
    else if (first >= 0xe0 && first <= 0xef) { code = first & 0x0f; continuation = 2; }
    else if (first >= 0xf0 && first <= 0xf4) { code = first & 0x07; continuation = 3; }
    else return false;
    if (continuation > value.size() - index) return false;
    for (std::size_t part = 0; part < continuation; ++part) {
      const auto next = static_cast<unsigned char>(value[index++]);
      if ((next & 0xc0) != 0x80) return false;
      code = (code << 6) | (next & 0x3f);
    }
    if ((continuation == 2 && code < 0x800) || (continuation == 3 && code < 0x10000) ||
        (code >= 0xd800 && code <= 0xdfff) || code > 0x10ffff) return false;
  }
  return true;
}

void parse_krl_extension(KrlReader* reader) {
  const auto name = reader->string();
  if (!valid_utf8(name)) throw std::runtime_error("KRL extension name is not UTF-8");
  const bool critical = reader->u8() != 0;
  (void)reader->string();
  if (critical) throw std::runtime_error("unsupported critical KRL extension");
}

std::string_view positive_mpint(std::string_view encoded) {
  if (!encoded.empty() && (static_cast<unsigned char>(encoded.front()) & 0x80) != 0)
    throw std::runtime_error("KRL bitmap mpint is negative");
  if (encoded.size() > 1 && encoded.front() == '\0') {
    if ((static_cast<unsigned char>(encoded[1]) & 0x80) == 0)
      throw std::runtime_error("KRL bitmap mpint has a non-canonical leading zero");
    return encoded.substr(1);
  }
  return encoded == std::string_view("\0", 1) ? std::string_view{} : encoded;
}

void parse_certificate_krl_section(KrlReader* reader, KrlData* krl) {
  KrlCertificateSection section;
  section.ca_key = std::string(reader->string());
  (void)reader->string();
  while (!reader->empty()) {
    const auto type = reader->u8();
    KrlReader data(reader->string());
    switch (type) {
      case 0x20: {  // serial list
        while (!data.empty()) {
          const auto serial = data.u64();
          if (serial == 0) throw std::runtime_error("KRL serial list contains zero");
          section.serial_ranges.emplace_back(serial, serial);
        }
        break;
      }
      case 0x21: {  // serial range
        const auto low = data.u64();
        const auto high = data.u64();
        if (low == 0 || low > high) throw std::runtime_error("KRL serial range is invalid");
        section.serial_ranges.emplace_back(low, high);
        break;
      }
      case 0x22: {  // serial bitmap
        KrlBitmap bitmap;
        bitmap.offset = data.u64();
        bitmap.bits = std::string(positive_mpint(data.string()));
        for (std::size_t byte_index = 0; byte_index < bitmap.bits.size(); ++byte_index) {
          const auto byte = static_cast<unsigned char>(bitmap.bits[bitmap.bits.size() - 1 - byte_index]);
          for (unsigned int bit = 0; bit < 8; ++bit) {
            if ((byte & (1U << bit)) == 0) continue;
            const auto index = byte_index * 8 + bit;
            if (index > std::numeric_limits<std::uint64_t>::max() - bitmap.offset)
              throw std::runtime_error("KRL serial overflow");
            if (bitmap.offset + index == 0) throw std::runtime_error("KRL bitmap revokes serial zero");
          }
        }
        section.serial_bitmaps.push_back(std::move(bitmap));
        break;
      }
      case 0x23: {  // key IDs
        while (!data.empty()) {
          const auto key_id = data.string();
          if (!valid_utf8(key_id)) throw std::runtime_error("KRL key ID is not UTF-8");
          section.key_ids.emplace(key_id);
        }
        break;
      }
      case 0x39:
        parse_krl_extension(&data);
        break;
      default:
        throw std::runtime_error("unsupported KRL certificate subsection " + std::to_string(type));
    }
    data.finish();
  }
  reader->finish();
  krl->certificate_sections.push_back(std::move(section));
}

bool canonical_positive_mpint(std::string_view value) {
  if (value.empty() || std::all_of(value.begin(), value.end(), [](char byte) { return byte == '\0'; }))
    return false;
  const auto first = static_cast<unsigned char>(value.front());
  if (first & 0x80) return false;
  if (first == 0 && (value.size() < 2 ||
      (static_cast<unsigned char>(value[1]) & 0x80) == 0)) return false;
  return true;
}

bool canonical_signature(std::string_view signature_blob) {
  std::size_t offset = 0;
  std::string_view algorithm, signature;
  if (!read_string(signature_blob, &offset, &algorithm) ||
      !read_string(signature_blob, &offset, &signature) || offset != signature_blob.size()) return false;
  if (algorithm.rfind("ecdsa-", 0) != 0) return true;
  std::size_t signature_offset = 0;
  std::string_view r, s;
  return read_string(signature, &signature_offset, &r) && canonical_positive_mpint(r) &&
      read_string(signature, &signature_offset, &s) && canonical_positive_mpint(s) &&
      signature_offset == signature.size();
}

KrlData parse_krl(std::string_view bytes, const std::vector<std::string>& trusted_signers,
                  std::string_view ca_blob) {
  if (bytes.size() > kMaxKrlBytes) throw std::runtime_error("KRL exceeds the 16 MiB limit");
  KrlReader header(bytes);
  if (header.exact(kKrlMagic.size()) != kKrlMagic) throw std::runtime_error("KRL magic is missing");
  if (header.u32() != 1) throw std::runtime_error("unsupported KRL format version");
  (void)header.u64();  // KRL version
  (void)header.u64();  // generated timestamp
  if (header.u64() != 0) throw std::runtime_error("KRL carries unsupported flags");
  (void)header.string();
  (void)header.string();

  KrlData krl;
  std::vector<KrlSignature> signatures;
  std::unordered_set<std::string> signature_keys;
  bool signatures_started = false;
  while (!header.empty()) {
    const auto type = header.u8();
    if (type == 4) {
      signatures_started = true;
      if (signatures.size() >= 16) throw std::runtime_error("KRL contains more than 16 signatures");
      KrlSignature signature;
      signature.key_blob = std::string(header.string());
      signature.signed_length = header.offset();
      signature.signature_blob = std::string(header.string());
      if (!parse_key_blob(signature.key_blob))
        throw std::runtime_error("KRL signature public key is invalid or unsupported");
      if (!canonical_signature(signature.signature_blob))
        throw std::runtime_error("KRL signature encoding is not canonical");
      if (!signature_keys.insert(signature.key_blob).second)
        throw std::runtime_error("KRL is signed more than once by the same key");
      signatures.push_back(std::move(signature));
      continue;
    }
    if (signatures_started)
      throw std::runtime_error("KRL contains a non-signature section after a signature");
    KrlReader section(header.string());
    switch (type) {
      case 1:
        parse_certificate_krl_section(&section, &krl);
        break;
      case 2:
        while (!section.empty()) {
          const auto key = section.string();
          if (key.empty()) throw std::runtime_error("KRL contains an empty explicit key");
          krl.explicit_keys.emplace(key);
        }
        break;
      case 3:
      case 5: {
        auto& fingerprints = type == 3 ? krl.sha1_fingerprints : krl.sha256_fingerprints;
        const std::size_t expected_length = type == 3 ? 20 : 32;
        while (!section.empty()) {
          const auto fingerprint = section.string();
          if (fingerprint.size() != expected_length)
            throw std::runtime_error("KRL fingerprint must be " + std::to_string(expected_length) + " bytes");
          fingerprints.emplace(fingerprint);
        }
        break;
      }
      case 255:
        parse_krl_extension(&section);
        break;
      default:
        throw std::runtime_error("unsupported KRL section " + std::to_string(type));
    }
    section.finish();
  }

  if (signatures.empty()) return krl;
  bool trusted_signature_found = false;
  for (std::size_t index = 0; index < signatures.size(); ++index) {
    const auto& signature = signatures[index];
    auto key = parse_key_blob(signature.key_blob);
    if (!verify_signature(key.get(), signature.key_blob,
            bytes.substr(0, signature.signed_length), signature.signature_blob)) {
      throw std::runtime_error("KRL signature " + std::to_string(index + 1) + " is invalid");
    }
    if (signature.key_blob == ca_blob) trusted_signature_found = true;
    for (const auto& trusted_line : trusted_signers) {
      std::string trusted_blob;
      if (!parse_authorized_key(trusted_line, &trusted_blob))
        throw std::runtime_error("trusted KRL signer public key is invalid or unsupported");
      if (signature.key_blob == trusted_blob) trusted_signature_found = true;
    }
  }
  if (!trusted_signature_found)
    throw std::runtime_error("signed KRL is not signed by the configured CA or a trusted KRL signer");
  return krl;
}

struct KrlUrl {
  bool https = false;
  std::string host;
  int port = 0;
  std::string path;
};

std::optional<KrlUrl> parse_krl_url(std::string_view value, std::string* error) {
  const auto reject = [error](const char* message) -> std::optional<KrlUrl> {
    if (error) *error = message;
    return std::nullopt;
  };
  if (value.empty() || value.size() > 4096 ||
      std::any_of(value.begin(), value.end(), [](unsigned char ch) { return ch <= 0x20 || ch == 0x7f; }) ||
      value.find_first_of("?#") != std::string_view::npos) {
    return reject("host certificate KRL URL is malformed");
  }
  KrlUrl result;
  const auto scheme_end = value.find("://");
  if (scheme_end == std::string_view::npos) {
    return reject("host certificate KRL URL scheme is invalid");
  }
  std::string scheme(value.substr(0, scheme_end));
  std::transform(scheme.begin(), scheme.end(), scheme.begin(), [](unsigned char ch) {
    return static_cast<char>(ascii_lower(ch));
  });
  if (scheme == "https") {
    result.https = true;
    result.port = 443;
  } else if (scheme == "http") {
    result.port = 80;
  } else {
    return reject("host certificate KRL URL must use HTTPS; HTTP is allowed only on loopback");
  }
  const std::string_view rest = value.substr(scheme_end + 3);
  const auto path_start = rest.find('/');
  const auto authority = rest.substr(0, path_start);
  result.path = path_start == std::string_view::npos ? "/" : std::string(rest.substr(path_start));
  if (authority.empty() || authority.find('@') != std::string_view::npos)
    return reject("host certificate KRL URL must have a host and no credentials");
  std::string_view port_text;
  if (authority.front() == '[') {
    const auto close = authority.find(']');
    if (close == std::string_view::npos || close == 1)
      return reject("host certificate KRL URL contains an invalid IPv6 host");
    result.host = std::string(authority.substr(1, close - 1));
    if (close + 1 < authority.size()) {
      if (authority[close + 1] != ':') return reject("host certificate KRL URL port is invalid");
      port_text = authority.substr(close + 2);
    }
  } else {
    const auto colon = authority.rfind(':');
    if (colon != std::string_view::npos) {
      if (authority.find(':') != colon) return reject("IPv6 hosts in KRL URLs must be bracketed");
      result.host = std::string(authority.substr(0, colon));
      port_text = authority.substr(colon + 1);
    } else {
      result.host = std::string(authority);
    }
  }
  if (!port_text.empty()) {
    unsigned int port = 0;
    const auto parsed = std::from_chars(port_text.data(), port_text.data() + port_text.size(), port);
    if (parsed.ec != std::errc{} || parsed.ptr != port_text.data() + port_text.size() ||
        port == 0 || port > 65535) return reject("host certificate KRL URL port is invalid");
    result.port = static_cast<int>(port);
  } else if ((!authority.empty() && authority.back() == ':') ||
             (authority.front() == '[' && authority.back() == ':')) {
    return reject("host certificate KRL URL port is empty");
  }
  if (result.host.empty() || result.host.size() > 253 || result.path.empty() || result.path.front() != '/')
    return reject("host certificate KRL URL is invalid");
  if (error) error->clear();
  return result;
}

bool is_loopback_host_impl(std::string_view host) {
  std::string lowered(host);
  std::transform(lowered.begin(), lowered.end(), lowered.begin(), [](unsigned char ch) {
    return static_cast<char>(ascii_lower(ch));
  });
  if (lowered == "localhost" || lowered.ends_with(".localhost")) return true;

  std::array<unsigned int, 4> ipv4{};
  std::size_t start = 0;
  bool valid_ipv4 = true;
  for (std::size_t index = 0; index < ipv4.size(); ++index) {
    const auto end = lowered.find('.', start);
    if ((index + 1 < ipv4.size()) != (end != std::string::npos)) {
      valid_ipv4 = false;
      break;
    }
    const std::string_view octet(lowered.data() + start,
        (end == std::string::npos ? lowered.size() : end) - start);
    if (octet.empty() || octet.size() > 3) {
      valid_ipv4 = false;
      break;
    }
    const auto parsed = std::from_chars(octet.data(), octet.data() + octet.size(), ipv4[index]);
    if (parsed.ec != std::errc{} || parsed.ptr != octet.data() + octet.size() || ipv4[index] > 255) {
      valid_ipv4 = false;
      break;
    }
    start = end == std::string::npos ? lowered.size() : end + 1;
  }
  if (valid_ipv4 && start == lowered.size()) return ipv4[0] == 127;

  if (lowered.empty() || lowered.find('%') != std::string::npos) return false;
  const auto compression = lowered.find("::");
  if (compression != std::string::npos && lowered.find("::", compression + 2) != std::string::npos) {
    return false;
  }
  const auto parse_side = [](std::string_view side, std::vector<std::uint16_t>* groups) {
    if (!groups) return false;
    if (side.empty()) return true;
    std::size_t offset = 0;
    while (offset <= side.size()) {
      const auto end = side.find(':', offset);
      const std::string_view token = side.substr(
          offset, end == std::string_view::npos ? side.size() - offset : end - offset);
      if (token.empty() || token.size() > 4) return false;
      unsigned int value = 0;
      const auto parsed = std::from_chars(token.data(), token.data() + token.size(), value, 16);
      if (parsed.ec != std::errc{} || parsed.ptr != token.data() + token.size() || value > 0xffff) {
        return false;
      }
      groups->push_back(static_cast<std::uint16_t>(value));
      if (end == std::string_view::npos) break;
      offset = end + 1;
    }
    return true;
  };
  std::vector<std::uint16_t> left;
  std::vector<std::uint16_t> right;
  if (compression == std::string::npos) {
    if (!parse_side(lowered, &left) || left.size() != 8) return false;
  } else if (!parse_side(std::string_view(lowered).substr(0, compression), &left) ||
             !parse_side(std::string_view(lowered).substr(compression + 2), &right) ||
             left.size() + right.size() >= 8) {
    return false;
  }
  std::array<std::uint16_t, 8> groups{};
  std::copy(left.begin(), left.end(), groups.begin());
  std::copy(right.begin(), right.end(), groups.end() - static_cast<std::ptrdiff_t>(right.size()));
  return std::all_of(groups.begin(), groups.end() - 1,
                     [](std::uint16_t group) { return group == 0; }) && groups.back() == 1;
}

void wipe_secret(std::string* value) {
  volatile char* bytes = value->empty() ? nullptr : value->data();
  for (std::size_t index = 0; index < value->size(); ++index) bytes[index] = '\0';
  value->clear();
}

bool write_socket_bytes(socket_t socket, std::string_view bytes) {
  std::size_t offset = 0;
  while (offset < bytes.size()) {
    const int chunk = static_cast<int>(std::min<std::size_t>(
        bytes.size() - offset, static_cast<std::size_t>(std::numeric_limits<int>::max())));
#ifdef MSG_NOSIGNAL
    constexpr int send_flags = MSG_NOSIGNAL;
#else
    constexpr int send_flags = 0;
#endif
    const int written = ::send(socket, bytes.data() + offset, chunk, send_flags);
    if (written <= 0) return false;
    offset += static_cast<std::size_t>(written);
  }
  return true;
}

bool read_socket_bytes(socket_t socket, char* bytes, std::size_t length) {
  std::size_t offset = 0;
  while (offset < length) {
    const int chunk = static_cast<int>(std::min<std::size_t>(
        length - offset, static_cast<std::size_t>(std::numeric_limits<int>::max())));
    const int received = ::recv(socket, bytes + offset, chunk, 0);
    if (received <= 0) return false;
    offset += static_cast<std::size_t>(received);
  }
  return true;
}

bool append_socks5_krl_destination(const KrlUrl& url,
                                   yukinal::ssh_service::ProxyProtocol proxy_protocol,
                                   std::string* request) {
  if (!request) return false;
  std::array<unsigned char, 16> address{};
  if (inet_pton(AF_INET, url.host.c_str(), address.data()) == 1) {
    request->push_back('\x01');
    request->append(reinterpret_cast<const char*>(address.data()), 4);
  } else if (inet_pton(AF_INET6, url.host.c_str(), address.data()) == 1) {
    request->push_back('\x04');
    request->append(reinterpret_cast<const char*>(address.data()), 16);
  } else if (proxy_protocol == yukinal::ssh_service::ProxyProtocol::socks5_remote_dns) {
    if (url.host.empty() || url.host.size() > 255 || url.host.find(':') != std::string::npos) {
      return false;
    }
    request->push_back('\x03');
    request->push_back(static_cast<char>(url.host.size()));
    request->append(url.host);
  } else {
    addrinfo hints{};
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    addrinfo* raw_addresses = nullptr;
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(10);
    const auto resolution = yukinal::ssh_service::detail::resolve_address_list(
        url.host, std::to_string(url.port), hints, deadline, &raw_addresses);
    std::unique_ptr<addrinfo, decltype(&freeaddrinfo)> addresses(raw_addresses, freeaddrinfo);
    if (resolution != yukinal::ssh_service::detail::AddressResolution::resolved || !addresses) {
      return false;
    }
    const addrinfo* selected = addresses.get();
    while (selected && selected->ai_family != AF_INET && selected->ai_family != AF_INET6) {
      selected = selected->ai_next;
    }
    if (!selected) return false;
    if (selected->ai_family == AF_INET) {
      const auto* ipv4 = reinterpret_cast<const sockaddr_in*>(selected->ai_addr);
      request->push_back('\x01');
      request->append(reinterpret_cast<const char*>(&ipv4->sin_addr), sizeof(ipv4->sin_addr));
    } else {
      const auto* ipv6 = reinterpret_cast<const sockaddr_in6*>(selected->ai_addr);
      request->push_back('\x04');
      request->append(reinterpret_cast<const char*>(&ipv6->sin6_addr), sizeof(ipv6->sin6_addr));
    }
  }
  request->push_back(static_cast<char>((url.port >> 8) & 0xff));
  request->push_back(static_cast<char>(url.port & 0xff));
  return true;
}

bool establish_krl_socks5_tunnel(socket_t socket, const KrlUrl& url,
                                 const yukinal::ssh_service::TargetConfig& target) {
  const bool auth_required = !target.proxy_auth_env.empty();
  const std::string greeting = auth_required ? std::string("\x05\x01\x02", 3)
                                             : std::string("\x05\x01\x00", 3);
  if (!write_socket_bytes(socket, greeting)) return false;
  std::array<unsigned char, 2> method_response{};
  if (!read_socket_bytes(socket, reinterpret_cast<char*>(method_response.data()), method_response.size()) ||
      method_response[0] != 5 || method_response[1] == 0xff) {
    return false;
  }
  if (auth_required) {
    if (method_response[1] != 2) return false;
    const char* configured = std::getenv(target.proxy_auth_env.c_str());
    std::string credential = configured ? configured : "";
    const auto separator = credential.find(':');
    if (credential.empty() || credential.size() > 1024 || separator == std::string::npos ||
        separator == 0 || separator + 1 >= credential.size() || separator > 255 ||
        credential.size() - separator - 1 > 255) {
      wipe_secret(&credential);
      return false;
    }
    std::string auth;
    auth.reserve(3 + credential.size());
    auth.push_back('\x01');
    auth.push_back(static_cast<char>(separator));
    auth.append(credential, 0, separator);
    auth.push_back(static_cast<char>(credential.size() - separator - 1));
    auth.append(credential, separator + 1, std::string::npos);
    wipe_secret(&credential);
    const bool sent = write_socket_bytes(socket, auth);
    wipe_secret(&auth);
    if (!sent) return false;
    std::array<unsigned char, 2> auth_response{};
    if (!read_socket_bytes(socket, reinterpret_cast<char*>(auth_response.data()), auth_response.size()) ||
        auth_response[0] != 1 || auth_response[1] != 0) {
      return false;
    }
  } else if (method_response[1] != 0) {
    return false;
  }

  std::string request("\x05\x01\x00", 3);
  if (!append_socks5_krl_destination(url, target.proxy_protocol, &request) ||
      !write_socket_bytes(socket, request)) {
    return false;
  }
  std::array<unsigned char, 4> response{};
  if (!read_socket_bytes(socket, reinterpret_cast<char*>(response.data()), response.size()) ||
      response[0] != 5 || response[1] != 0 || response[2] != 0) {
    return false;
  }
  std::size_t address_size = 0;
  if (response[3] == 1) address_size = 4;
  else if (response[3] == 4) address_size = 16;
  else if (response[3] == 3) {
    unsigned char length = 0;
    if (!read_socket_bytes(socket, reinterpret_cast<char*>(&length), 1)) return false;
    address_size = length;
  } else {
    return false;
  }
  std::array<char, 257> bound_address{};
  return address_size <= 255 &&
      read_socket_bytes(socket, bound_address.data(), address_size + 2);
}

class SocksKrlHttpsClient final : public httplib::ClientImpl {
 public:
  SocksKrlHttpsClient(const KrlUrl& url,
                      const yukinal::ssh_service::TargetConfig& target)
      : httplib::ClientImpl(url.host, url.port), url_(url), target_(target),
        context_(SSL_CTX_new(TLS_client_method()), SSL_CTX_free) {
    set_keep_alive(false);
    if (context_) {
      SSL_CTX_set_min_proto_version(context_.get(), TLS1_2_VERSION);
      SSL_CTX_set_verify(context_.get(), SSL_VERIFY_PEER, nullptr);
      const char* ca_file = std::getenv("SSL_CERT_FILE");
      const int loaded = ca_file && *ca_file
          ? SSL_CTX_load_verify_locations(context_.get(), ca_file, nullptr)
          : SSL_CTX_set_default_verify_paths(context_.get());
      if (loaded != 1) context_.reset();
    }
  }

  bool is_valid() const override { return context_ != nullptr; }

 protected:
  bool create_and_connect_socket(Socket& socket, httplib::Error& error) override {
    socket.sock = httplib::detail::create_client_socket(
        target_.proxy_host, "", target_.proxy_port, address_family_, tcp_nodelay_,
        ipv6_v6only_, socket_options_, connection_timeout_sec_, connection_timeout_usec_,
        read_timeout_sec_, read_timeout_usec_, write_timeout_sec_, write_timeout_usec_,
        interface_, error);
    if (socket.sock == INVALID_SOCKET) return false;
    if (!establish_krl_socks5_tunnel(socket.sock, url_, target_)) {
      error = httplib::Error::Connection;
      httplib::detail::close_socket(socket.sock);
      socket.sock = INVALID_SOCKET;
      return false;
    }
    return true;
  }

  bool process_socket(const Socket& socket,
                      std::function<bool(httplib::Stream&)> callback) override {
    std::unique_ptr<SSL, decltype(&SSL_free)> ssl(SSL_new(context_.get()), SSL_free);
    if (!ssl || SSL_set_fd(ssl.get(), static_cast<int>(socket.sock)) != 1) return false;
    X509_VERIFY_PARAM* verify = SSL_get0_param(ssl.get());
    std::array<unsigned char, 16> address{};
    const bool is_ip = inet_pton(AF_INET, url_.host.c_str(), address.data()) == 1 ||
        inet_pton(AF_INET6, url_.host.c_str(), address.data()) == 1;
    if (is_ip) {
      if (X509_VERIFY_PARAM_set1_ip_asc(verify, url_.host.c_str()) != 1) return false;
    } else {
      if (SSL_set_tlsext_host_name(ssl.get(), url_.host.c_str()) != 1 ||
          X509_VERIFY_PARAM_set1_host(verify, url_.host.c_str(), 0) != 1) {
        return false;
      }
    }
    if (SSL_connect(ssl.get()) != 1 || SSL_get_verify_result(ssl.get()) != X509_V_OK) return false;
    return httplib::detail::process_client_socket_ssl(
        ssl.get(), socket.sock, read_timeout_sec_, read_timeout_usec_,
        write_timeout_sec_, write_timeout_usec_, std::move(callback));
  }

 private:
  KrlUrl url_;
  yukinal::ssh_service::TargetConfig target_;
  std::unique_ptr<SSL_CTX, decltype(&SSL_CTX_free)> context_;
};

bool krl_uses_proxy(const yukinal::ssh_service::TargetConfig& target,
                    const KrlUrl& url) {
  if (target.proxy_host.empty()) return false;
  yukinal::ssh_service::TargetConfig proxy_target = target;
  proxy_target.host = url.host;
  proxy_target.port = url.port;
  return !yukinal::ssh_service::proxy_bypasses_target(proxy_target);
}

bool krl_uses_socks_proxy(const yukinal::ssh_service::TargetConfig& target,
                          const KrlUrl& url) {
  return krl_uses_proxy(target, url) &&
      (target.proxy_protocol == yukinal::ssh_service::ProxyProtocol::socks5_local_dns ||
       target.proxy_protocol == yukinal::ssh_service::ProxyProtocol::socks5_remote_dns);
}

template <typename Client>
std::optional<std::string> perform_krl_http_request(Client& client, const KrlUrl& url,
    const yukinal::ssh_service::TargetConfig& target, std::string* error,
    bool socks_proxy_tunnel = false) {
  client.set_connection_timeout(std::chrono::seconds(10));
  client.set_read_timeout(std::chrono::seconds(15));
  client.set_write_timeout(std::chrono::seconds(15));
  client.set_follow_location(false);
  const bool use_proxy = krl_uses_proxy(target, url);
  const bool socks_proxy = krl_uses_socks_proxy(target, url);
  if (socks_proxy && !url.https) {
    if (error) *error = "HTTP KRL downloads must bypass the SOCKS proxy and use loopback directly";
    return std::nullopt;
  }
  if (socks_proxy != socks_proxy_tunnel) {
    if (error) *error = "the host certificate KRL proxy route could not be established";
    return std::nullopt;
  }
  std::string credential;
  bool proxy_auth_configured = false;
  if (use_proxy && !socks_proxy_tunnel) {
    client.set_proxy(target.proxy_host, target.proxy_port);
    if (!target.proxy_auth_env.empty()) {
      const char* value = std::getenv(target.proxy_auth_env.c_str());
      if (!value || !*value || std::strlen(value) > 1024) {
        if (error) *error = "proxy authentication credential is unavailable";
        return std::nullopt;
      }
      credential = value;
      const auto separator = credential.find(':');
      if (separator == std::string::npos) {
        wipe_secret(&credential);
        if (error) *error = "proxy authentication credential is invalid";
        return std::nullopt;
      }
      client.set_proxy_basic_auth(credential.substr(0, separator), credential.substr(separator + 1));
      proxy_auth_configured = true;
    }
  }
  std::string body;
  bool oversized = false;
  auto response = client.Get(url.path,
      [&](const httplib::Response& headers) {
        const auto content_length = headers.get_header_value("Content-Length");
        if (!content_length.empty()) {
          std::uint64_t parsed_length = 0;
          const auto parsed = std::from_chars(content_length.data(),
              content_length.data() + content_length.size(), parsed_length);
          if (parsed.ec == std::errc{} && parsed.ptr == content_length.data() + content_length.size() &&
              parsed_length > kMaxKrlBytes) {
            oversized = true;
            return false;
          }
        }
        return true;
      },
      [&](const char* data, std::size_t length) {
        if (length > kMaxKrlBytes - body.size()) {
          oversized = true;
          return false;
        }
        body.append(data, length);
        return true;
      });
  if (proxy_auth_configured) client.set_proxy_basic_auth("", "");
  if (!credential.empty()) wipe_secret(&credential);
  if (oversized) {
    if (error) *error = "host certificate KRL exceeds the 16 MiB limit";
    return std::nullopt;
  }
  if (!response) {
    if (error) *error = "host certificate KRL request failed";
    return std::nullopt;
  }
  if (response->status < 200 || response->status >= 300) {
    if (error) *error = "host certificate KRL URL returned a non-success HTTP status";
    return std::nullopt;
  }
  if (body.empty()) {
    if (error) *error = "host certificate KRL response is empty";
    return std::nullopt;
  }
  return body;
}

bool key_is_revoked(const KrlData& krl, std::string_view key_blob) {
  if (krl.explicit_keys.contains(std::string(key_blob))) return true;
  std::array<unsigned char, 32> digest{};
  unsigned int digest_length = 0;
  if (EVP_Digest(key_blob.data(), key_blob.size(), digest.data(), &digest_length, EVP_sha1(), nullptr) == 1 &&
      digest_length == 20 && krl.sha1_fingerprints.contains(
          std::string(reinterpret_cast<const char*>(digest.data()), digest_length))) return true;
  digest_length = 0;
  if (EVP_Digest(key_blob.data(), key_blob.size(), digest.data(), &digest_length, EVP_sha256(), nullptr) == 1 &&
      digest_length == digest.size() && krl.sha256_fingerprints.contains(
          std::string(reinterpret_cast<const char*>(digest.data()), digest_length))) return true;
  return false;
}

const char* revoked_reason(const KrlData& krl, std::string_view subject_key,
                           std::string_view ca_key, std::string_view key_id,
                           std::uint64_t serial) {
  if (key_is_revoked(krl, subject_key)) return "certificate public key is revoked";
  if (key_is_revoked(krl, ca_key)) return "certificate signing CA is revoked";
  for (const auto& section : krl.certificate_sections) {
    if (!section.ca_key.empty() && section.ca_key != ca_key) continue;
    if (section.key_ids.contains(std::string(key_id))) return "certificate key ID is revoked";
    if (serial != 0 && std::any_of(section.serial_ranges.begin(), section.serial_ranges.end(),
        [serial](const auto& range) { return range.first <= serial && serial <= range.second; }))
      return "certificate serial is revoked";
    if (serial != 0 && std::any_of(section.serial_bitmaps.begin(), section.serial_bitmaps.end(),
        [serial](const KrlBitmap& bitmap) {
          if (serial < bitmap.offset) return false;
          const auto bit_index = serial - bitmap.offset;
          const auto byte_index = static_cast<std::size_t>(bit_index / 8);
          if (byte_index >= bitmap.bits.size()) return false;
          const auto byte = static_cast<unsigned char>(bitmap.bits[bitmap.bits.size() - 1 - byte_index]);
          return (byte & (1U << (bit_index % 8))) != 0;
        })) return "certificate serial is revoked";
  }
  return nullptr;
}

}  // namespace

bool is_loopback_host(std::string_view host) {
  return is_loopback_host_impl(host);
}

bool valid_host_ca_public_key(std::string_view ca_public_key) {
  return host_ca_public_key_blob(ca_public_key).has_value();
}

std::optional<std::string> host_ca_public_key_blob(std::string_view ca_public_key) {
  std::string blob;
  if (!parse_authorized_key(ca_public_key, &blob)) return std::nullopt;
  return blob;
}

bool verify_host_certificate(std::string_view blob, std::string_view ca_public_key,
                             const std::vector<std::string>& allowed_principals,
                             std::string* error) {
  return verify_host_certificate_with_krl(blob, ca_public_key, allowed_principals,
                                          {}, {}, error);
}

bool verify_host_certificate_with_krl(std::string_view blob, std::string_view ca_public_key,
                                      const std::vector<std::string>& allowed_principals,
                                      std::string_view revocation_list,
                                      const std::vector<std::string>& revocation_signers,
                                      std::string* error) {
  const auto reject = [error](const char* message) {
    if (error) *error = message;
    return false;
  };
  if (blob.empty() || blob.size() > kMaxCertificateBytes) return reject("host certificate is invalid or too large");
  if (allowed_principals.empty() || allowed_principals.size() > 256 ||
      std::any_of(allowed_principals.begin(), allowed_principals.end(), [](const auto& principal) {
        return principal.empty() || principal.size() > 253;
      })) return reject("host CA principals are invalid");
  std::string ca_blob;
  if (!parse_authorized_key(ca_public_key, &ca_blob)) return reject("host CA public key is invalid or unsupported");
  auto ca_key = parse_key_blob(ca_blob);
  std::vector<std::string> certificate_principals;
  std::uint64_t valid_after = 0, valid_before = 0;
  std::uint64_t serial = 0;
  std::string_view key_id;
  std::string subject_key_blob;
  std::string_view critical_options, certificate_ca_blob, signature_blob;
  std::size_t signed_length = 0;
  std::uint32_t certificate_type = 0;
  if (!parse_certificate(blob, &certificate_principals, &valid_after, &valid_before,
                         &critical_options, &certificate_ca_blob, &signature_blob,
                         &signed_length, &certificate_type, &serial, &key_id,
                         &subject_key_blob))
    return reject("host certificate is malformed");
  if (!valid_utf8(key_id) || std::any_of(certificate_principals.begin(), certificate_principals.end(),
      [](const auto& principal) { return !valid_utf8(principal); }))
    return reject("host certificate contains invalid UTF-8 metadata");
  if (certificate_type != 2) return reject("certificate is not a host certificate");
  if (certificate_ca_blob != ca_blob) return reject("certificate is signed by a different CA");
  if (!verify_signature(ca_key.get(), ca_blob, blob.substr(0, signed_length), signature_blob))
    return reject("host certificate signature is invalid");
  if (!critical_options.empty()) return reject("host certificate carries unsupported critical options");
  const auto now = std::chrono::duration_cast<std::chrono::seconds>(
      std::chrono::system_clock::now().time_since_epoch()).count();
  if (now < 0) return reject("system clock is before the Unix epoch");
  if (static_cast<std::uint64_t>(now) < valid_after) return reject("host certificate is not valid yet");
  if (static_cast<std::uint64_t>(now) > valid_before) return reject("host certificate has expired");
  const bool principal_match = std::any_of(certificate_principals.begin(), certificate_principals.end(),
      [&](const auto& certificate_principal) {
        return std::any_of(allowed_principals.begin(), allowed_principals.end(),
            [&](const auto& allowed) { return principal_matches(allowed, certificate_principal); });
      });
  if (!principal_match) return reject("host certificate principal does not match configured host patterns");
  if (!revocation_list.empty()) {
    try {
      const KrlData krl = parse_krl(revocation_list, revocation_signers, ca_blob);
      if (const char* reason = revoked_reason(krl, subject_key_blob, certificate_ca_blob,
                                               key_id, serial)) return reject(reason);
    } catch (const std::exception& exception) {
      if (error) *error = std::string("host certificate revocation list is invalid: ") + exception.what();
      return false;
    }
  }
  if (error) error->clear();
  return true;
}

std::optional<std::string> load_host_revocation_list(
    const yukinal::ssh_service::TargetConfig& target, std::string* error) {
  if (error) error->clear();
  if (!target.host_ca_revocation_list_path.empty()) {
    const std::filesystem::path path(target.host_ca_revocation_list_path);
    if (!path.is_absolute()) {
      if (error) *error = "host certificate KRL path must be absolute";
      return std::nullopt;
    }
    std::ifstream input(path, std::ios::binary);
    if (!input) {
      if (error) *error = "could not open the configured host certificate KRL";
      return std::nullopt;
    }
    std::string bytes;
    std::array<char, 8192> buffer{};
    while (input) {
      input.read(buffer.data(), static_cast<std::streamsize>(buffer.size()));
      const auto count = static_cast<std::size_t>(input.gcount());
      if (count > kMaxKrlBytes - bytes.size()) {
        if (error) *error = "host certificate KRL exceeds the 16 MiB limit";
        return std::nullopt;
      }
      bytes.append(buffer.data(), count);
    }
    if (!input.eof()) {
      if (error) *error = "could not read the configured host certificate KRL";
      return std::nullopt;
    }
    if (bytes.empty()) {
      if (error) *error = "configured host certificate KRL is empty";
      return std::nullopt;
    }
    return bytes;
  }
  if (target.host_ca_revocation_list_url.empty()) return std::string{};

  auto url = parse_krl_url(target.host_ca_revocation_list_url, error);
  if (!url) return std::nullopt;
  if (!url->https && !is_loopback_host(url->host)) {
    if (error) *error = "host certificate KRL URL must use HTTPS; HTTP is allowed only on loopback";
    return std::nullopt;
  }
  if (url->https) {
    if (krl_uses_socks_proxy(target, *url)) {
      SocksKrlHttpsClient client(*url, target);
      if (!client.is_valid()) {
        if (error) *error = "could not create a verified HTTPS client for the host certificate KRL";
        return std::nullopt;
      }
      return perform_krl_http_request(client, *url, target, error, true);
    }
    httplib::SSLClient client(url->host, url->port);
    if (!client.is_valid()) {
      if (error) *error = "could not create an HTTPS client for the host certificate KRL";
      return std::nullopt;
    }
    if (const char* ca_file = std::getenv("SSL_CERT_FILE"); ca_file && *ca_file) {
      client.set_ca_cert_path(ca_file);
    }
    return perform_krl_http_request(client, *url, target, error);
  }
  httplib::Client client(url->host, url->port);
  return perform_krl_http_request(client, *url, target, error);
}

}  // namespace yukinal::ssh_service::detail

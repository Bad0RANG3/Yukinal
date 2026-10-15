#include "host_key.hpp"

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <memory>
#include <string>
#include <string_view>
#include <vector>

#include <openssl/bn.h>
#include <openssl/ec.h>
#include <openssl/ecdsa.h>
#include <openssl/evp.h>
#include <openssl/rsa.h>

namespace yukinal::ssh_service::detail {
namespace {

constexpr std::size_t kMaxCertificateBytes = 1024 * 1024;
constexpr std::string_view kCertificateSuffix = "-cert-v01@openssh.com";

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
                       std::uint32_t* certificate_type) {
  std::size_t offset = 0;
  std::string_view algorithm, nonce;
  if (!read_string(blob, &offset, &algorithm) || algorithm.size() <= kCertificateSuffix.size() ||
      algorithm.substr(algorithm.size() - kCertificateSuffix.size()) != kCertificateSuffix ||
      !read_string(blob, &offset, &nonce) || nonce.size() < 16 || nonce.size() > 1024) return false;
  const auto plain = algorithm.substr(0, algorithm.size() - kCertificateSuffix.size());
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
  std::uint64_t serial = 0;
  std::string_view key_id, principal_blob;
  if (!read_u64(blob, &offset, &serial) || !read_u32(blob, &offset, certificate_type) ||
      !read_string(blob, &offset, &key_id) || key_id.size() > 4096 ||
      !read_string(blob, &offset, &principal_blob) || !parse_principal_list(principal_blob, principals) ||
      !read_u64(blob, &offset, valid_after) || !read_u64(blob, &offset, valid_before) ||
      !read_string(blob, &offset, critical_options)) return false;
  std::string_view ignored;
  if (!read_string(blob, &offset, &ignored) || !read_string(blob, &offset, &ignored) ||
      !read_string(blob, &offset, ca_blob)) return false;
  *signed_length = offset;
  return read_string(blob, &offset, signature_blob) && offset == blob.size();
}

}  // namespace

bool valid_host_ca_public_key(std::string_view ca_public_key) {
  std::string blob;
  return parse_authorized_key(ca_public_key, &blob);
}

bool verify_host_certificate(std::string_view blob, std::string_view ca_public_key,
                             const std::vector<std::string>& allowed_principals,
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
  std::string_view critical_options, certificate_ca_blob, signature_blob;
  std::size_t signed_length = 0;
  std::uint32_t certificate_type = 0;
  if (!parse_certificate(blob, &certificate_principals, &valid_after, &valid_before,
                         &critical_options, &certificate_ca_blob, &signature_blob,
                         &signed_length, &certificate_type))
    return reject("host certificate is malformed");
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
  if (error) error->clear();
  return true;
}

}  // namespace yukinal::ssh_service::detail

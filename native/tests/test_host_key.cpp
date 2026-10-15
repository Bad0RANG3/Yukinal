#include "test_support.hpp"

#include "host_key.hpp"

#include <string>
#include <string_view>
#include <chrono>
#include <cstdint>

#include <openssl/evp.h>

namespace {

void append_string(std::string_view value, std::string* output) {
  const auto size = static_cast<unsigned int>(value.size());
  output->push_back(static_cast<char>((size >> 24) & 0xffU));
  output->push_back(static_cast<char>((size >> 16) & 0xffU));
  output->push_back(static_cast<char>((size >> 8) & 0xffU));
  output->push_back(static_cast<char>(size & 0xffU));
  output->append(value);
}

std::string ed25519_key(std::string_view algorithm, std::string_view public_key) {
  std::string key;
  append_string(algorithm, &key);
  append_string(public_key, &key);
  return key;
}

std::string ed25519_certificate(std::string_view public_key) {
  std::string certificate;
  append_string("ssh-ed25519-cert-v01@openssh.com", &certificate);
  append_string(std::string(16, 'n'), &certificate);
  append_string(public_key, &certificate);
  return certificate;
}

void append_u32(std::uint32_t value, std::string* output) {
  output->push_back(static_cast<char>(value >> 24));
  output->push_back(static_cast<char>(value >> 16));
  output->push_back(static_cast<char>(value >> 8));
  output->push_back(static_cast<char>(value));
}

void append_u64(std::uint64_t value, std::string* output) {
  for (int shift = 56; shift >= 0; shift -= 8) output->push_back(static_cast<char>(value >> shift));
}

std::string encode_base64(std::string_view value) {
  std::string output(4 * ((value.size() + 2) / 3), '\0');
  const int length = EVP_EncodeBlock(reinterpret_cast<unsigned char*>(output.data()),
      reinterpret_cast<const unsigned char*>(value.data()), static_cast<int>(value.size()));
  output.resize(static_cast<std::size_t>(length));
  return output;
}

struct SignedHostCertificate {
  std::string ca_public_key_line;
  std::string certificate;
};

SignedHostCertificate make_signed_host_certificate(std::string_view principal = "api.example.test",
    std::uint32_t certificate_type = 2, std::string_view critical_options = {},
    std::int64_t valid_after_offset = -60, std::int64_t valid_before_offset = 3600,
    char ca_seed = 'c') {
  const std::string seed(32, ca_seed);
  EVP_PKEY* private_key = EVP_PKEY_new_raw_private_key(EVP_PKEY_ED25519, nullptr,
      reinterpret_cast<const unsigned char*>(seed.data()), seed.size());
  YUKINAL_REQUIRE(private_key != nullptr);
  std::string public_key(32, '\0');
  std::size_t public_key_size = public_key.size();
  YUKINAL_REQUIRE(EVP_PKEY_get_raw_public_key(private_key,
      reinterpret_cast<unsigned char*>(public_key.data()), &public_key_size) == 1);
  public_key.resize(public_key_size);
  const auto ca_blob = ed25519_key("ssh-ed25519", public_key);
  const auto ca_line = "ssh-ed25519 " + encode_base64(ca_blob) + " test-ca";

  const auto now = std::chrono::duration_cast<std::chrono::seconds>(
      std::chrono::system_clock::now().time_since_epoch()).count();
  std::string certificate;
  append_string("ssh-ed25519-cert-v01@openssh.com", &certificate);
  append_string(std::string(16, 'n'), &certificate);
  append_string(std::string(32, 'h'), &certificate);
  append_u64(7, &certificate);
  append_u32(certificate_type, &certificate);
  append_string("test host", &certificate);
  std::string principals;
  append_string(principal, &principals);
  append_string(principals, &certificate);
  append_u64(static_cast<std::uint64_t>(now + valid_after_offset), &certificate);
  append_u64(static_cast<std::uint64_t>(now + valid_before_offset), &certificate);
  append_string(critical_options, &certificate);
  append_string({}, &certificate);
  append_string({}, &certificate);
  append_string(ca_blob, &certificate);

  EVP_MD_CTX* context = EVP_MD_CTX_new();
  YUKINAL_REQUIRE(context != nullptr);
  YUKINAL_REQUIRE(EVP_DigestSignInit(context, nullptr, nullptr, nullptr, private_key) == 1);
  std::string raw_signature(64, '\0');
  std::size_t signature_size = raw_signature.size();
  YUKINAL_REQUIRE(EVP_DigestSign(context,
      reinterpret_cast<unsigned char*>(raw_signature.data()), &signature_size,
      reinterpret_cast<const unsigned char*>(certificate.data()), certificate.size()) == 1);
  raw_signature.resize(signature_size);
  EVP_MD_CTX_free(context);
  EVP_PKEY_free(private_key);
  std::string signature;
  append_string("ssh-ed25519", &signature);
  append_string(raw_signature, &signature);
  append_string(signature, &certificate);
  return {ca_line, certificate};
}

}  // namespace

YUKINAL_TEST(host_certificate_normalizes_to_its_certified_ed25519_key) {
  const std::string public_key(32, 'k');
  const auto certificate = ed25519_certificate(public_key);
  const auto expected = ed25519_key("ssh-ed25519", public_key);
  const auto material = yukinal::ssh_service::detail::normalize_host_key_blob(certificate);

  YUKINAL_REQUIRE(material.has_value());
  YUKINAL_REQUIRE(material->certificate);
  YUKINAL_REQUIRE(material->algorithm ==
                  yukinal::ssh_service::detail::HostKeyAlgorithm::ed25519);
  YUKINAL_REQUIRE(material->public_key_blob == expected);
  const auto certificate_fingerprint =
      yukinal::ssh_service::detail::fingerprint_host_key_blob(certificate);
  const auto key_fingerprint =
      yukinal::ssh_service::detail::fingerprint_host_key_blob(expected);
  YUKINAL_REQUIRE(!certificate_fingerprint.empty());
  YUKINAL_REQUIRE(certificate_fingerprint == key_fingerprint);
}

YUKINAL_TEST(host_certificate_normalization_handles_rsa_and_ecdsa_fields) {
  using yukinal::ssh_service::detail::HostKeyAlgorithm;

  std::string rsa_certificate;
  append_string("ssh-rsa-cert-v01@openssh.com", &rsa_certificate);
  append_string(std::string(16, 'n'), &rsa_certificate);
  append_string(std::string("\x01\x00\x01", 3), &rsa_certificate);
  append_string(std::string("\x00\x80", 2), &rsa_certificate);
  const auto rsa = yukinal::ssh_service::detail::normalize_host_key_blob(rsa_certificate);
  YUKINAL_REQUIRE(rsa.has_value());
  YUKINAL_REQUIRE(rsa->algorithm == HostKeyAlgorithm::rsa);
  std::string expected_rsa;
  append_string("ssh-rsa", &expected_rsa);
  append_string(std::string("\x01\x00\x01", 3), &expected_rsa);
  append_string(std::string("\x00\x80", 2), &expected_rsa);
  YUKINAL_REQUIRE(rsa->public_key_blob == expected_rsa);
  YUKINAL_REQUIRE(!yukinal::ssh_service::detail::fingerprint_host_key_blob(expected_rsa).empty());

  std::string ecdsa_certificate;
  append_string("ecdsa-sha2-nistp256-cert-v01@openssh.com", &ecdsa_certificate);
  append_string(std::string(16, 'n'), &ecdsa_certificate);
  append_string("nistp256", &ecdsa_certificate);
  append_string(std::string("\x04\x01\x02", 3), &ecdsa_certificate);
  const auto ecdsa = yukinal::ssh_service::detail::normalize_host_key_blob(ecdsa_certificate);
  YUKINAL_REQUIRE(ecdsa.has_value());
  YUKINAL_REQUIRE(ecdsa->algorithm == HostKeyAlgorithm::ecdsa_256);
  std::string expected_ecdsa;
  append_string("ecdsa-sha2-nistp256", &expected_ecdsa);
  append_string("nistp256", &expected_ecdsa);
  append_string(std::string("\x04\x01\x02", 3), &expected_ecdsa);
  YUKINAL_REQUIRE(ecdsa->public_key_blob == expected_ecdsa);
}

YUKINAL_TEST(host_key_normalization_rejects_truncated_or_invalid_certificates) {
  std::string truncated;
  append_string("ssh-ed25519-cert-v01@openssh.com", &truncated);
  append_string("short", &truncated);
  YUKINAL_REQUIRE(!yukinal::ssh_service::detail::normalize_host_key_blob(truncated));

  std::string wrong_curve;
  append_string("ecdsa-sha2-nistp256-cert-v01@openssh.com", &wrong_curve);
  append_string(std::string(16, 'n'), &wrong_curve);
  append_string("nistp384", &wrong_curve);
  append_string("point", &wrong_curve);
  YUKINAL_REQUIRE(!yukinal::ssh_service::detail::normalize_host_key_blob(wrong_curve));

  YUKINAL_REQUIRE(yukinal::ssh_service::detail::fingerprint_host_key_blob("not-an-ssh-key").empty());
}

YUKINAL_TEST(host_ca_accepts_a_valid_signed_ed25519_host_certificate) {
  const auto fixture = make_signed_host_certificate("API.EXAMPLE.TEST");
  std::string error;
  YUKINAL_REQUIRE(yukinal::ssh_service::detail::verify_host_certificate(
      fixture.certificate, fixture.ca_public_key_line, {"*.example.test"}, &error));
  YUKINAL_REQUIRE(error.empty());
}

YUKINAL_TEST(host_ca_rejects_a_wrong_principal_ca_and_tampered_signature) {
  const auto fixture = make_signed_host_certificate();
  std::string error;
  YUKINAL_REQUIRE(!yukinal::ssh_service::detail::verify_host_certificate(
      fixture.certificate, fixture.ca_public_key_line, {"other.example.test"}, &error));
  YUKINAL_REQUIRE(!yukinal::ssh_service::detail::verify_host_certificate(
      fixture.certificate, make_signed_host_certificate("api.example.test", 2, {}, -60, 3600, 'd').ca_public_key_line,
      {"*.example.test"}, &error));
  auto tampered = fixture.certificate;
  tampered.back() ^= 1;
  YUKINAL_REQUIRE(!yukinal::ssh_service::detail::verify_host_certificate(
      tampered, fixture.ca_public_key_line, {"*.example.test"}, &error));
}

YUKINAL_TEST(host_ca_rejects_user_expired_future_and_critical_certificates) {
  std::string error;
  const auto user_certificate = make_signed_host_certificate("api.example.test", 1);
  YUKINAL_REQUIRE(!yukinal::ssh_service::detail::verify_host_certificate(
      user_certificate.certificate, user_certificate.ca_public_key_line,
      {"*.example.test"}, &error));

  const auto expired_certificate = make_signed_host_certificate("api.example.test", 2, {}, -3600, -10);
  YUKINAL_REQUIRE(!yukinal::ssh_service::detail::verify_host_certificate(
      expired_certificate.certificate, expired_certificate.ca_public_key_line,
      {"*.example.test"}, &error));

  const auto future_certificate = make_signed_host_certificate("api.example.test", 2, {}, 3600, 7200);
  YUKINAL_REQUIRE(!yukinal::ssh_service::detail::verify_host_certificate(
      future_certificate.certificate, future_certificate.ca_public_key_line,
      {"*.example.test"}, &error));

  const auto critical_certificate = make_signed_host_certificate("api.example.test", 2, "unsupported-option");
  YUKINAL_REQUIRE(!yukinal::ssh_service::detail::verify_host_certificate(
      critical_certificate.certificate, critical_certificate.ca_public_key_line,
      {"*.example.test"}, &error));
}

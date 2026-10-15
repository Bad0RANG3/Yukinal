#include "test_support.hpp"

#include "host_key.hpp"

#include <string>
#include <string_view>

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

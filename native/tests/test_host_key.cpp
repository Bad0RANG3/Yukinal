#include "test_support.hpp"

#include "host_key.hpp"

#include <string>
#include <string_view>
#include <chrono>
#include <cstdint>
#include <filesystem>
#include <fstream>
#include <thread>
#include <utility>
#include <vector>

#include <openssl/evp.h>
#include <httplib.h>
#include "yukinal/config.hpp"

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

std::string encode_base64(std::string_view value);

struct Ed25519Signer {
  std::string seed;
  std::string public_key;
  std::string public_blob;
  std::string public_key_line;
};

Ed25519Signer make_signer(char seed_byte) {
  Ed25519Signer signer;
  signer.seed.assign(32, seed_byte);
  EVP_PKEY* private_key = EVP_PKEY_new_raw_private_key(EVP_PKEY_ED25519, nullptr,
      reinterpret_cast<const unsigned char*>(signer.seed.data()), signer.seed.size());
  YUKINAL_REQUIRE(private_key != nullptr);
  signer.public_key.resize(32, '\0');
  std::size_t public_key_size = signer.public_key.size();
  YUKINAL_REQUIRE(EVP_PKEY_get_raw_public_key(private_key,
      reinterpret_cast<unsigned char*>(signer.public_key.data()), &public_key_size) == 1);
  signer.public_key.resize(public_key_size);
  EVP_PKEY_free(private_key);
  signer.public_blob = ed25519_key("ssh-ed25519", signer.public_key);
  signer.public_key_line = "ssh-ed25519 " + encode_base64(signer.public_blob) + " test-ca";
  return signer;
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
  std::string ca_public_key_blob;
  std::string ca_seed;
  std::string certificate;
};

SignedHostCertificate make_signed_host_certificate(std::string_view principal = "api.example.test",
    std::uint32_t certificate_type = 2, std::string_view critical_options = {},
    std::int64_t valid_after_offset = -60, std::int64_t valid_before_offset = 3600,
    char ca_seed = 'c') {
  const auto signer = make_signer(ca_seed);
  EVP_PKEY* private_key = EVP_PKEY_new_raw_private_key(EVP_PKEY_ED25519, nullptr,
      reinterpret_cast<const unsigned char*>(signer.seed.data()), signer.seed.size());
  YUKINAL_REQUIRE(private_key != nullptr);
  const auto& ca_blob = signer.public_blob;

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
  return {signer.public_key_line, ca_blob, signer.seed, certificate};
}

std::string make_unsigned_krl(const std::vector<std::pair<std::uint8_t, std::string>>& sections) {
  std::string krl("SSHKRL\n\0", 8);
  append_u32(1, &krl);
  append_u64(1, &krl);
  append_u64(0, &krl);
  append_u64(0, &krl);
  append_string({}, &krl);
  append_string("test KRL", &krl);
  for (const auto& [type, body] : sections) {
    krl.push_back(static_cast<char>(type));
    append_string(body, &krl);
  }
  return krl;
}

std::string make_krl(const std::vector<std::pair<std::uint8_t, std::string>>& sections,
                     const std::vector<Ed25519Signer>& signers) {
  std::string krl = make_unsigned_krl(sections);
  for (const auto& signer : signers) {
    krl.push_back(4);
    append_string(signer.public_blob, &krl);
    EVP_PKEY* private_key = EVP_PKEY_new_raw_private_key(EVP_PKEY_ED25519, nullptr,
        reinterpret_cast<const unsigned char*>(signer.seed.data()), signer.seed.size());
    YUKINAL_REQUIRE(private_key != nullptr);
    EVP_MD_CTX* context = EVP_MD_CTX_new();
    YUKINAL_REQUIRE(context != nullptr);
    YUKINAL_REQUIRE(EVP_DigestSignInit(context, nullptr, nullptr, nullptr, private_key) == 1);
    std::string raw_signature(64, '\0');
    std::size_t signature_size = raw_signature.size();
    YUKINAL_REQUIRE(EVP_DigestSign(context,
        reinterpret_cast<unsigned char*>(raw_signature.data()), &signature_size,
        reinterpret_cast<const unsigned char*>(krl.data()), krl.size()) == 1);
    raw_signature.resize(signature_size);
    EVP_MD_CTX_free(context);
    EVP_PKEY_free(private_key);
    std::string signature;
    append_string("ssh-ed25519", &signature);
    append_string(raw_signature, &signature);
    append_string(signature, &krl);
  }
  return krl;
}

std::string make_krl(const std::vector<std::pair<std::uint8_t, std::string>>& sections,
                     const Ed25519Signer& signer) {
  return make_krl(sections, std::vector<Ed25519Signer>{signer});
}

std::string certificate_krl_section(std::string_view ca_blob,
                                    std::uint8_t subsection_type,
                                    std::string_view subsection_data) {
  std::string section;
  append_string(ca_blob, &section);
  append_string({}, &section);
  section.push_back(static_cast<char>(subsection_type));
  append_string(subsection_data, &section);
  return section;
}

std::string digest(std::string_view value, const EVP_MD* algorithm) {
  std::string result(static_cast<std::size_t>(EVP_MD_get_size(algorithm)), '\0');
  unsigned int length = 0;
  YUKINAL_REQUIRE(EVP_Digest(value.data(), value.size(),
      reinterpret_cast<unsigned char*>(result.data()), &length, algorithm, nullptr) == 1);
  result.resize(length);
  return result;
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

YUKINAL_TEST(host_krl_rejects_certificate_serial_ranges_bitmaps_and_key_ids) {
  const auto fixture = make_signed_host_certificate();
  const auto signer = make_signer('c');
  std::string error;
  const auto assert_revoked = [&](std::uint8_t subsection_type, const std::string& data,
                                  std::string_view expected) {
    const auto certificate_section = certificate_krl_section(
        fixture.ca_public_key_blob, subsection_type, data);
    const auto krl = make_krl({{1, certificate_section}}, signer);
    YUKINAL_REQUIRE(!yukinal::ssh_service::detail::verify_host_certificate_with_krl(
        fixture.certificate, fixture.ca_public_key_line, {"*.example.test"}, krl, {}, &error));
    YUKINAL_REQUIRE(error.find(expected) != std::string::npos);
  };

  std::string serial_list;
  append_u64(7, &serial_list);
  assert_revoked(0x20, serial_list, "serial is revoked");

  std::string serial_range;
  append_u64(6, &serial_range);
  append_u64(8, &serial_range);
  assert_revoked(0x21, serial_range, "serial is revoked");

  std::string bitmap;
  append_u64(7, &bitmap);
  append_string(std::string(1, '\x01'), &bitmap);
  assert_revoked(0x22, bitmap, "serial is revoked");

  std::string key_ids;
  append_string("test host", &key_ids);
  assert_revoked(0x23, key_ids, "key ID is revoked");
}

YUKINAL_TEST(host_krl_rejects_explicit_keys_and_sha1_sha256_fingerprints) {
  const auto fixture = make_signed_host_certificate();
  const auto signer = make_signer('c');
  const std::string subject_key = ed25519_key("ssh-ed25519", std::string(32, 'h'));
  std::string error;
  const auto assert_revoked = [&](std::uint8_t section_type, const std::string& data,
                                  std::string_view expected) {
    const auto krl = make_krl({{section_type, data}}, signer);
    YUKINAL_REQUIRE(!yukinal::ssh_service::detail::verify_host_certificate_with_krl(
        fixture.certificate, fixture.ca_public_key_line, {"*.example.test"}, krl, {}, &error));
    YUKINAL_REQUIRE(error.find(expected) != std::string::npos);
  };

  std::string explicit_subject;
  append_string(subject_key, &explicit_subject);
  assert_revoked(2, explicit_subject, "public key is revoked");

  std::string sha1_subject;
  append_string(digest(subject_key, EVP_sha1()), &sha1_subject);
  assert_revoked(3, sha1_subject, "public key is revoked");

  std::string sha256_subject;
  append_string(digest(subject_key, EVP_sha256()), &sha256_subject);
  assert_revoked(5, sha256_subject, "public key is revoked");

  std::string explicit_ca;
  append_string(fixture.ca_public_key_blob, &explicit_ca);
  assert_revoked(2, explicit_ca, "signing CA is revoked");
}

YUKINAL_TEST(host_krl_validates_signatures_and_allows_configured_signer_rotation) {
  const auto fixture = make_signed_host_certificate();
  const auto ca = make_signer('c');
  const auto old_signer = make_signer('d');
  const auto new_signer = make_signer('e');
  std::string error;
  const auto unsigned_krl = make_unsigned_krl({});
  YUKINAL_REQUIRE(yukinal::ssh_service::detail::verify_host_certificate_with_krl(
      fixture.certificate, fixture.ca_public_key_line, {"*.example.test"}, unsigned_krl, {}, &error));

  const auto signed_krl = make_krl({}, std::vector<Ed25519Signer>{old_signer, new_signer});
  YUKINAL_REQUIRE(yukinal::ssh_service::detail::verify_host_certificate_with_krl(
      fixture.certificate, fixture.ca_public_key_line, {"*.example.test"}, signed_krl,
      {new_signer.public_key_line}, &error));
  YUKINAL_REQUIRE(!yukinal::ssh_service::detail::verify_host_certificate_with_krl(
      fixture.certificate, fixture.ca_public_key_line, {"*.example.test"}, signed_krl, {}, &error));
  YUKINAL_REQUIRE(error.find("trusted") != std::string::npos);

  const auto ca_signed_krl = make_krl({}, ca);
  const bool valid_ca_signature = yukinal::ssh_service::detail::verify_host_certificate_with_krl(
      fixture.certificate, fixture.ca_public_key_line, {"*.example.test"}, ca_signed_krl, {}, &error);
  YUKINAL_REQUIRE(valid_ca_signature);
  auto tampered = ca_signed_krl;
  tampered[20] ^= 1;
  YUKINAL_REQUIRE(!yukinal::ssh_service::detail::verify_host_certificate_with_krl(
      fixture.certificate, fixture.ca_public_key_line, {"*.example.test"}, tampered, {}, &error));
  YUKINAL_REQUIRE(error.find("signature") != std::string::npos);
}

YUKINAL_TEST(host_krl_rejects_critical_extensions_and_malformed_sections) {
  const auto fixture = make_signed_host_certificate();
  std::string error;
  std::string extension;
  append_string("critical@example", &extension);
  extension.push_back(1);
  append_string("body", &extension);
  const auto critical_krl = make_unsigned_krl({{255, extension}});
  YUKINAL_REQUIRE(!yukinal::ssh_service::detail::verify_host_certificate_with_krl(
      fixture.certificate, fixture.ca_public_key_line, {"*.example.test"}, critical_krl, {}, &error));
  YUKINAL_REQUIRE(error.find("critical") != std::string::npos);

  const auto unknown_krl = make_unsigned_krl({{77, {}}});
  YUKINAL_REQUIRE(!yukinal::ssh_service::detail::verify_host_certificate_with_krl(
      fixture.certificate, fixture.ca_public_key_line, {"*.example.test"}, unknown_krl, {}, &error));
  YUKINAL_REQUIRE(!yukinal::ssh_service::detail::verify_host_certificate_with_krl(
      fixture.certificate, fixture.ca_public_key_line, {"*.example.test"}, "not a KRL", {}, &error));
}

YUKINAL_TEST(host_krl_loader_bounds_local_files_and_restricts_http_urls) {
  const auto bytes = make_unsigned_krl({});
  const auto temporary_path = std::filesystem::temp_directory_path() /
      ("yukinal-krl-" + std::to_string(std::chrono::steady_clock::now().time_since_epoch().count()));
  {
    std::ofstream output(temporary_path, std::ios::binary);
    YUKINAL_REQUIRE(output.good());
    output.write(bytes.data(), static_cast<std::streamsize>(bytes.size()));
  }
  yukinal::ssh_service::TargetConfig target;
  target.host_ca_revocation_list_path = temporary_path.string();
  std::string error;
  const auto loaded = yukinal::ssh_service::detail::load_host_revocation_list(target, &error);
  std::filesystem::remove(temporary_path);
  YUKINAL_REQUIRE(loaded.has_value());
  YUKINAL_REQUIRE(*loaded == bytes);

  target.host_ca_revocation_list_path.clear();
  target.host_ca_revocation_list_url = "http://example.test/revoked.krl";
  YUKINAL_REQUIRE(!yukinal::ssh_service::detail::load_host_revocation_list(target, &error));
  YUKINAL_REQUIRE(error.find("HTTPS") != std::string::npos);

  target.host_ca_revocation_list_url = "https://user:password@example.test/revoked.krl";
  YUKINAL_REQUIRE(!yukinal::ssh_service::detail::load_host_revocation_list(target, &error));
  YUKINAL_REQUIRE(error.find("no credentials") != std::string::npos);

  target.host_ca_revocation_list_url = "https://example.test/revoked.krl?token=secret";
  YUKINAL_REQUIRE(!yukinal::ssh_service::detail::load_host_revocation_list(target, &error));
  YUKINAL_REQUIRE(error.find("malformed") != std::string::npos);
}

YUKINAL_TEST(host_krl_loader_fetches_loopback_http_responses) {
  const auto expected = make_unsigned_krl({});
  httplib::Server server;
  server.Get("/revoked.krl", [&](const httplib::Request&, httplib::Response& response) {
    response.set_content(expected, "application/octet-stream");
  });
  const int port = server.bind_to_any_port("127.0.0.1");
  YUKINAL_REQUIRE(port > 0);
  std::thread listener([&server] { server.listen_after_bind(); });
  for (int attempt = 0; attempt < 100 && !server.is_running(); ++attempt)
    std::this_thread::sleep_for(std::chrono::milliseconds(10));

  yukinal::ssh_service::TargetConfig target;
  target.host_ca_revocation_list_url = "http://127.0.0.1:" + std::to_string(port) + "/revoked.krl";
  std::string error;
  const auto downloaded = yukinal::ssh_service::detail::load_host_revocation_list(target, &error);
  server.stop();
  listener.join();
  YUKINAL_REQUIRE(downloaded.has_value());
  YUKINAL_REQUIRE(*downloaded == expected);
}

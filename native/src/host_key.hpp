#pragma once

#include <optional>
#include <string>
#include <string_view>
#include <vector>

namespace yukinal::ssh_service {
struct TargetConfig;
}

namespace yukinal::ssh_service::detail {

enum class HostKeyAlgorithm {
  rsa,
  dss,
  ecdsa_256,
  ecdsa_384,
  ecdsa_521,
  ed25519,
};

struct HostKeyMaterial {
  std::string public_key_blob;
  HostKeyAlgorithm algorithm;
  bool certificate = false;
};

// Returns the plain public key represented by a host-key blob. For OpenSSH
// certificates, this deliberately extracts the certified key; callers must
// still apply their configured pin or known-hosts policy.
std::optional<HostKeyMaterial> normalize_host_key_blob(std::string_view blob);
bool valid_host_ca_public_key(std::string_view ca_public_key);
std::optional<std::string> host_ca_public_key_blob(std::string_view ca_public_key);
bool is_loopback_host(std::string_view host);

// Validates an OpenSSH host certificate against an authorized_keys-style CA
// public key line and the configured host principal patterns.
bool verify_host_certificate(std::string_view blob,
                             std::string_view ca_public_key,
                             const std::vector<std::string>& principals,
                             std::string* error = nullptr);

bool verify_host_certificate_with_krl(std::string_view blob,
                                      std::string_view ca_public_key,
                                      const std::vector<std::string>& principals,
                                      std::string_view revocation_list,
                                      const std::vector<std::string>& revocation_signers,
                                      std::string* error = nullptr);

std::optional<std::string> load_host_revocation_list(
    const yukinal::ssh_service::TargetConfig& target, std::string* error = nullptr);

// Fingerprints the underlying public key, matching OpenSSH certificate
// fingerprint semantics and the desktop host-key trust store.
std::string fingerprint_host_key_blob(std::string_view blob);

}  // namespace yukinal::ssh_service::detail

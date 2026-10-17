#include "yukinal/desktop_host/desktop_oauth.hpp"

#include "yukinal/desktop_host/credential_store.hpp"
#include "yukinal/desktop_host/database.hpp"

#include <httplib.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cctype>
#include <cstdint>
#include <iomanip>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <random>
#include <sstream>
#include <stdexcept>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#ifdef CPPHTTPLIB_OPENSSL_SUPPORT
#include <openssl/evp.h>
#include <openssl/x509.h>
#endif

namespace yukinal::desktop_host {
namespace {

// ---------------------------------------------------------------------------
// Small string / URL helpers shared by both build shapes.

std::string trim_ascii(std::string value) {
  std::size_t start = 0;
  while (start < value.size() && std::isspace(static_cast<unsigned char>(value[start])) != 0) {
    ++start;
  }
  std::size_t end = value.size();
  while (end > start && std::isspace(static_cast<unsigned char>(value[end - 1])) != 0) {
    --end;
  }
  return value.substr(start, end - start);
}

// Mirrors Rust's `char::is_control`: C0, DEL and the C1 range.
bool has_control_characters(const std::string& value) {
  for (std::size_t index = 0; index < value.size(); ++index) {
    const auto byte = static_cast<unsigned char>(value[index]);
    if (byte < 0x20 || byte == 0x7f) return true;
    if (byte == 0xc2 && index + 1 < value.size()) {
      const auto next = static_cast<unsigned char>(value[index + 1]);
      if (next >= 0x80 && next <= 0x9f) return true;
    }
  }
  return false;
}

std::string ascii_lower(std::string value) {
  std::transform(value.begin(), value.end(), value.begin(),
                 [](unsigned char byte) { return static_cast<char>(std::tolower(byte)); });
  return value;
}

std::string ascii_upper(std::string value) {
  std::transform(value.begin(), value.end(), value.begin(),
                 [](unsigned char byte) { return static_cast<char>(std::toupper(byte)); });
  return value;
}

// One line of remote text that ends up in an error message: controls removed and
// bounded, the same treatment the retired Rust used before showing a server string.
std::string sanitize(const std::string& value) {
  std::string out;
  out.reserve(std::min<std::size_t>(value.size(), 512));
  for (const char character : value) {
    if (out.size() >= 512) break;
    const auto byte = static_cast<unsigned char>(character);
    if (byte < 0x20 || byte == 0x7f) continue;
    out.push_back(character);
  }
  return out;
}

std::string text_field(const nlohmann::json& value, const char* key,
                       const std::string& fallback = std::string()) {
  if (value.is_object() && value.contains(key) && value.at(key).is_string()) {
    return value.at(key).get<std::string>();
  }
  return fallback;
}

bool bool_field(const nlohmann::json& value, const char* key, bool fallback) {
  if (value.is_object() && value.contains(key) && value.at(key).is_boolean()) {
    return value.at(key).get<bool>();
  }
  return fallback;
}

std::vector<std::string> scope_list(const nlohmann::json& oauth) {
  std::vector<std::string> scopes;
  if (oauth.is_object() && oauth.contains("scopes") && oauth.at("scopes").is_array()) {
    for (const auto& item : oauth.at("scopes")) {
      if (item.is_string()) scopes.push_back(item.get<std::string>());
    }
  }
  return scopes;
}

std::string join(const std::vector<std::string>& values, const char* separator) {
  std::string out;
  for (const auto& value : values) {
    if (!out.empty()) out += separator;
    out += value;
  }
  return out;
}

std::uint64_t unix_now() {
  return static_cast<std::uint64_t>(
      std::chrono::duration_cast<std::chrono::seconds>(
          std::chrono::system_clock::now().time_since_epoch())
          .count());
}

std::string iso8601_from_epoch(std::uint64_t epoch_seconds) {
  const auto time = static_cast<std::time_t>(epoch_seconds);
  std::tm utc{};
#ifdef _WIN32
  gmtime_s(&utc, &time);
#else
  gmtime_r(&time, &utc);
#endif
  std::ostringstream rendered;
  rendered << std::put_time(&utc, "%Y-%m-%dT%H:%M:%SZ");
  return rendered.str();
}

std::string next_id(const char* prefix) {
  static std::atomic<std::uint64_t> counter{1};
  const auto millis = std::chrono::duration_cast<std::chrono::milliseconds>(
                          std::chrono::system_clock::now().time_since_epoch())
                          .count();
  std::ostringstream rendered;
  rendered << prefix << '_' << std::hex << static_cast<std::uint64_t>(millis)
           << counter.fetch_add(1, std::memory_order_relaxed);
  return rendered.str();
}

std::string base64_encode(const unsigned char* data, std::size_t size, const char* alphabet,
                          bool padding) {
  std::string out;
  out.reserve((size + 2) / 3 * 4);
  for (std::size_t index = 0; index < size; index += 3) {
    const std::uint32_t first = data[index];
    const std::uint32_t second = index + 1 < size ? data[index + 1] : 0;
    const std::uint32_t third = index + 2 < size ? data[index + 2] : 0;
    const std::uint32_t value = (first << 16) | (second << 8) | third;
    out.push_back(alphabet[(value >> 18) & 63]);
    out.push_back(alphabet[(value >> 12) & 63]);
    if (index + 1 < size) {
      out.push_back(alphabet[(value >> 6) & 63]);
    } else if (padding) {
      out.push_back('=');
    }
    if (index + 2 < size) {
      out.push_back(alphabet[value & 63]);
    } else if (padding) {
      out.push_back('=');
    }
  }
  return out;
}

std::string base64url(const unsigned char* data, std::size_t size) {
  static const char kAlphabet[] =
      "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
  return base64_encode(data, size, kAlphabet, false);
}

std::string base64url(const std::string& value) {
  return base64url(reinterpret_cast<const unsigned char*>(value.data()), value.size());
}

std::string base64_standard(const std::string& value) {
  static const char kAlphabet[] =
      "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  return base64_encode(reinterpret_cast<const unsigned char*>(value.data()), value.size(),
                       kAlphabet, true);
}

bool decode_base64url(const std::string& value, std::vector<unsigned char>& out) {
  out.clear();
  out.reserve(value.size() * 3 / 4);
  std::uint32_t buffer = 0;
  std::uint32_t bits = 0;
  for (const char character : value) {
    const auto byte = static_cast<unsigned char>(character);
    if (byte == '=') continue;
    std::uint32_t digit = 0;
    if (byte >= 'A' && byte <= 'Z') {
      digit = byte - 'A';
    } else if (byte >= 'a' && byte <= 'z') {
      digit = byte - 'a' + 26;
    } else if (byte >= '0' && byte <= '9') {
      digit = byte - '0' + 52;
    } else if (byte == '-' || byte == '+') {
      digit = 62;
    } else if (byte == '_' || byte == '/') {
      digit = 63;
    } else {
      return false;
    }
    buffer = (buffer << 6) | digit;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out.push_back(static_cast<unsigned char>((buffer >> bits) & 0xff));
    }
  }
  return true;
}

std::string random_base64url(std::size_t bytes) {
  std::random_device device;
  std::vector<unsigned char> buffer(bytes);
  for (auto& byte : buffer) byte = static_cast<unsigned char>(device());
  return base64url(buffer.data(), buffer.size());
}

std::string url_encode(const std::string& value) {
  static const char kHex[] = "0123456789ABCDEF";
  std::string out;
  out.reserve(value.size());
  for (const char character : value) {
    const auto byte = static_cast<unsigned char>(character);
    if (std::isalnum(byte) != 0 || byte == '-' || byte == '_' || byte == '.' || byte == '~') {
      out.push_back(static_cast<char>(byte));
    } else {
      out.push_back('%');
      out.push_back(kHex[byte >> 4]);
      out.push_back(kHex[byte & 0x0f]);
    }
  }
  return out;
}

bool ipv4_loopback(const std::string& host) {
  int octets[4] = {0, 0, 0, 0};
  std::size_t start = 0;
  for (int index = 0; index < 4; ++index) {
    const auto dot = host.find('.', start);
    const auto end = dot == std::string::npos ? host.size() : dot;
    if (end == start || end - start > 3) return false;
    int value = 0;
    for (std::size_t position = start; position < end; ++position) {
      if (std::isdigit(static_cast<unsigned char>(host[position])) == 0) return false;
      value = value * 10 + (host[position] - '0');
    }
    if (value > 255) return false;
    octets[index] = value;
    if (index < 3) {
      if (dot == std::string::npos) return false;
      start = dot + 1;
    } else if (dot != std::string::npos) {
      return false;
    }
  }
  return octets[0] == 127;
}

bool is_loopback_host(std::string host) {
  if (host.size() >= 2 && host.front() == '[' && host.back() == ']') {
    host = host.substr(1, host.size() - 2);
  }
  const auto lowered = ascii_lower(host);
  if (lowered == "localhost") return true;
  if (lowered.size() > 10 && lowered.compare(lowered.size() - 10, 10, ".localhost") == 0) {
    return true;
  }
  if (lowered == "::1") return true;
  return ipv4_loopback(lowered);
}

// The same rule the settings validator applies: HTTPS anywhere, plain HTTP only on
// loopback, and never a query, fragment or embedded credentials.
bool validate_oauth_url(const std::string& raw, std::string& out) {
  const auto value = trim_ascii(raw);
  if (value.empty() || value.size() > 2048 || has_control_characters(value)) return false;
  if (value.find('?') != std::string::npos || value.find('#') != std::string::npos) return false;
  std::size_t authority_start = 0;
  bool secure = false;
  if (value.rfind("https://", 0) == 0) {
    authority_start = 8;
    secure = true;
  } else if (value.rfind("http://", 0) == 0) {
    authority_start = 7;
  } else {
    return false;
  }
  if (authority_start >= value.size()) return false;
  const auto path = value.find('/', authority_start);
  const auto authority = value.substr(
      authority_start, path == std::string::npos ? std::string::npos : path - authority_start);
  if (authority.empty() || authority.find('@') != std::string::npos) return false;
  std::string host = authority;
  if (!host.empty() && host.front() == '[') {
    const auto close = host.find(']');
    if (close == std::string::npos) return false;
    host = host.substr(0, close + 1);
  } else {
    const auto colon = host.find(':');
    if (colon != std::string::npos) host = host.substr(0, colon);
  }
  if (host.empty()) return false;
  if (!secure && !is_loopback_host(host)) return false;
  out = value;
  return true;
}

struct ParsedUrl {
  bool secure = false;
  std::string scheme;
  std::string host;  // IPv6 addresses keep their brackets
  std::string port;  // empty when the URL omits it
  std::string path;  // always begins with '/'
};

bool parse_url(const std::string& value, ParsedUrl& out) {
  bool secure = false;
  std::size_t base = 0;
  if (value.rfind("https://", 0) == 0) {
    secure = true;
    base = 8;
  } else if (value.rfind("http://", 0) == 0) {
    base = 7;
  } else {
    return false;
  }
  const auto authority_end = value.find_first_of("/?#", base);
  const std::string authority =
      value.substr(base, authority_end == std::string::npos ? std::string::npos
                                                            : authority_end - base);
  if (authority.empty() || authority.find('@') != std::string::npos) return false;
  std::string host;
  std::string port;
  if (authority.front() == '[') {
    const auto close = authority.find(']');
    if (close == std::string::npos) return false;
    host = authority.substr(0, close + 1);
    if (close + 1 < authority.size()) {
      if (authority.at(close + 1) != ':') return false;
      port = authority.substr(close + 2);
    }
  } else {
    const auto colon = authority.rfind(':');
    if (colon == std::string::npos) {
      host = authority;
    } else {
      host = authority.substr(0, colon);
      port = authority.substr(colon + 1);
    }
  }
  if (host.empty()) return false;
  std::string path = "/";
  if (authority_end != std::string::npos && value.at(authority_end) == '/') {
    const auto path_end = value.find_first_of("?#", authority_end);
    path = value.substr(authority_end, path_end == std::string::npos ? std::string::npos
                                                                     : path_end - authority_end);
    if (path.empty()) path = "/";
  }
  out.secure = secure;
  out.scheme = secure ? "https" : "http";
  out.host = host;
  out.port = port;
  out.path = path;
  return true;
}

// The `scheme://host:port` form httplib's Client wants.
std::string scheme_host_port(const ParsedUrl& url) {
  const auto port = url.port.empty() ? (url.secure ? "443" : "80") : url.port;
  return url.scheme + "://" + url.host + ":" + port;
}

// RFC 9449 `htu`: scheme, host, explicit non-default port and path, no query or
// fragment.
std::string canonical_htu(const std::string& url) {
  ParsedUrl parsed;
  if (!parse_url(url, parsed)) return url;
  const bool default_port = parsed.port.empty() || (parsed.secure && parsed.port == "443") ||
                            (!parsed.secure && parsed.port == "80");
  const auto port_part = default_port ? std::string() : ":" + parsed.port;
  return parsed.scheme + "://" + parsed.host + port_part + parsed.path;
}

// The key the DPoP nonce cache is stored under: always carries the port, matching
// Rust's `Url::origin().ascii_serialization()`.
std::string origin_of(const std::string& url) {
  ParsedUrl parsed;
  if (!parse_url(url, parsed)) return std::string();
  const auto port = parsed.port.empty() ? (parsed.secure ? "443" : "80") : parsed.port;
  return parsed.scheme + "://" + parsed.host + ":" + port;
}

struct SimpleResponse {
  int status = 0;
  std::string body;
  httplib::Headers headers;
};

void configure_client(httplib::Client& client, int timeout_seconds) {
  client.set_connection_timeout(10, 0);
  client.set_read_timeout(timeout_seconds, 0);
  client.set_write_timeout(timeout_seconds, 0);
  client.set_follow_location(false);
}

SimpleResponse http_get(const std::string& url, const httplib::Headers& headers,
                        int timeout_seconds) {
  ParsedUrl parsed;
  if (!parse_url(url, parsed)) throw std::runtime_error("the OAuth request URL is invalid");
  httplib::Client client(scheme_host_port(parsed));
  configure_client(client, timeout_seconds);
  auto response = client.Get(parsed.path, headers);
  if (!response) {
    throw std::runtime_error("the OAuth request failed: " + httplib::to_string(response.error()));
  }
  return {response->status, response->body, response->headers};
}

SimpleResponse http_post_form(const std::string& url, const httplib::Headers& headers,
                              const httplib::Params& params, int timeout_seconds) {
  ParsedUrl parsed;
  if (!parse_url(url, parsed)) throw std::runtime_error("the OAuth request URL is invalid");
  httplib::Client client(scheme_host_port(parsed));
  configure_client(client, timeout_seconds);
  auto response = client.Post(parsed.path, headers, params);
  if (!response) {
    throw std::runtime_error("the OAuth request failed: " + httplib::to_string(response.error()));
  }
  return {response->status, response->body, response->headers};
}

SimpleResponse http_post_json(const std::string& url, const httplib::Headers& headers,
                              const std::string& body, int timeout_seconds) {
  ParsedUrl parsed;
  if (!parse_url(url, parsed)) throw std::runtime_error("the OAuth request URL is invalid");
  httplib::Client client(scheme_host_port(parsed));
  configure_client(client, timeout_seconds);
  auto response = client.Post(parsed.path, headers, body, "application/json");
  if (!response) {
    throw std::runtime_error("the OAuth request failed: " + httplib::to_string(response.error()));
  }
  return {response->status, response->body, response->headers};
}

}  // namespace

#ifdef CPPHTTPLIB_OPENSSL_SUPPORT
namespace {

// ---------------------------------------------------------------------------
// The OpenSSL-backed engine. The desktop host already links OpenSSL for the SSH
// service, so Ed25519 (RFC 9449 DPoP) and SHA-256 (PKCE and `ath`) come from it.

constexpr int kDiscoveryTimeoutSeconds = 10;
constexpr int kTokenTimeoutSeconds = 30;
constexpr std::size_t kMaxMetadataBytes = 256 * 1024;
constexpr std::size_t kMaxRegistrationResponseBytes = 64 * 1024;
constexpr std::size_t kMaxTokenResponseBytes = 64 * 1024;
constexpr long long kTokenExpirySkewSeconds = 60;
constexpr const char* kDynamicClientName = "Yukinal Desktop";
constexpr const char* kDeviceCodeGrant = "urn:ietf:params:oauth:grant-type:device_code";
constexpr int kDeviceDefaultPollSeconds = 5;
constexpr int kMaxDevicePolls = 600;
constexpr long long kDeviceMaxLifetimeSeconds = 30 * 60;
constexpr const char* kCancelledFlow = "OAuth authorization was cancelled";
const auto kCallbackTimeout = std::chrono::minutes(5);
const char* const kCallbackSuccessHtml =
    "<!doctype html><html><head><meta charset=\"utf-8\"><title>Yukinal OAuth</title></head>"
    "<body><p>OAuth authorization complete. You can close this window and return to "
    "Yukinal.</p></body></html>";
const char* const kCallbackFailureHtml =
    "<!doctype html><html><head><meta charset=\"utf-8\"><title>Yukinal OAuth</title></head>"
    "<body><p>OAuth authorization failed.</p></body></html>";

std::string sha256_bytes(const std::string& data) {
  unsigned char digest[EVP_MAX_MD_SIZE];
  unsigned int length = 0;
  if (EVP_Digest(data.data(), data.size(), digest, &length, EVP_sha256(), nullptr) != 1) {
    throw std::runtime_error("could not compute a SHA-256 digest");
  }
  return std::string(reinterpret_cast<const char*>(digest), length);
}

// One Ed25519 key: generated once per DPoP server, stored as base64url PKCS#8 in
// the OS credential store, and loaded back on every session.
class DpopKey {
 public:
  DpopKey() = default;
  ~DpopKey() {
    if (key_ != nullptr) EVP_PKEY_free(key_);
  }
  DpopKey(const DpopKey&) = delete;
  DpopKey& operator=(const DpopKey&) = delete;
  DpopKey(DpopKey&& other) noexcept : key_(other.key_), public_(std::move(other.public_)) {
    other.key_ = nullptr;
  }
  DpopKey& operator=(DpopKey&& other) noexcept {
    if (this != &other) {
      if (key_ != nullptr) EVP_PKEY_free(key_);
      key_ = other.key_;
      public_ = std::move(other.public_);
      other.key_ = nullptr;
    }
    return *this;
  }

  static DpopKey generate(std::string& encoded) {
    EVP_PKEY_CTX* context = EVP_PKEY_CTX_new_id(EVP_PKEY_ED25519, nullptr);
    if (context == nullptr) throw std::runtime_error("could not generate a DPoP key");
    EVP_PKEY* key = nullptr;
    const bool generated =
        EVP_PKEY_keygen_init(context) > 0 && EVP_PKEY_keygen(context, &key) > 0;
    EVP_PKEY_CTX_free(context);
    if (!generated || key == nullptr) {
      if (key != nullptr) EVP_PKEY_free(key);
      throw std::runtime_error("could not generate a DPoP key");
    }
    DpopKey result;
    result.key_ = key;
    result.public_ = raw_public(key);
    const auto der = encode_pkcs8(key);
    encoded = base64url(der.data(), der.size());
    return result;
  }

  static DpopKey load(const CredentialStore& credentials, const std::string& reference) {
    const auto parsed = CredentialReference::parse(reference);
    const auto secret = credentials.get(parsed);
    std::string encoded;
    if (secret.size() != 0) {
      encoded.assign(reinterpret_cast<const char*>(secret.data()),
                     reinterpret_cast<const char*>(secret.data()) + secret.size());
    }
    encoded = trim_ascii(encoded);
    std::vector<unsigned char> der;
    if (!decode_base64url(encoded, der) || der.empty()) {
      throw std::runtime_error("the stored DPoP key is not valid base64url");
    }
    const unsigned char* cursor = der.data();
    PKCS8_PRIV_KEY_INFO* info =
        d2i_PKCS8_PRIV_KEY_INFO(nullptr, &cursor, static_cast<long>(der.size()));
    if (info == nullptr) {
      throw std::runtime_error("the stored DPoP key is not a usable Ed25519 key");
    }
    EVP_PKEY* key = EVP_PKCS82PKEY(info);
    PKCS8_PRIV_KEY_INFO_free(info);
    if (key == nullptr) {
      throw std::runtime_error("the stored DPoP key is not a usable Ed25519 key");
    }
    DpopKey result;
    result.key_ = key;
    result.public_ = raw_public(key);
    return result;
  }

  [[nodiscard]] const std::string& public_key() const { return public_; }

  [[nodiscard]] std::string proof(const std::string& method, const std::string& url,
                                  const std::string& nonce,
                                  const std::string& access_token) const {
    nlohmann::json header = {{"typ", "dpop+jwt"},
                             {"alg", "EdDSA"},
                             {"jwk", {{"kty", "OKP"}, {"crv", "Ed25519"}, {"x", public_}}}};
    nlohmann::json claims;
    claims["jti"] = random_base64url(16);
    claims["htm"] = ascii_upper(method);
    claims["htu"] = canonical_htu(url);
    claims["iat"] = unix_now();
    if (!nonce.empty()) claims["nonce"] = nonce;
    if (!access_token.empty()) claims["ath"] = base64url(sha256_bytes(access_token));
    const auto signing_input = base64url(header.dump()) + "." + base64url(claims.dump());
    const auto signature = sign(signing_input);
    return signing_input + "." + base64url(signature.data(), signature.size());
  }

 private:
  static std::string raw_public(EVP_PKEY* key) {
    std::size_t length = 0;
    if (EVP_PKEY_get_raw_public_key(key, nullptr, &length) <= 0 || length == 0) {
      throw std::runtime_error("could not read the DPoP public key");
    }
    std::vector<unsigned char> buffer(length);
    if (EVP_PKEY_get_raw_public_key(key, buffer.data(), &length) <= 0) {
      throw std::runtime_error("could not read the DPoP public key");
    }
    return base64url(buffer.data(), length);
  }

  static std::vector<unsigned char> encode_pkcs8(EVP_PKEY* key) {
    PKCS8_PRIV_KEY_INFO* info = EVP_PKEY2PKCS8(key);
    if (info == nullptr) throw std::runtime_error("could not encode the DPoP key");
    const int length = i2d_PKCS8_PRIV_KEY_INFO(info, nullptr);
    if (length <= 0) {
      PKCS8_PRIV_KEY_INFO_free(info);
      throw std::runtime_error("could not encode the DPoP key");
    }
    std::vector<unsigned char> der(static_cast<std::size_t>(length));
    unsigned char* cursor = der.data();
    if (i2d_PKCS8_PRIV_KEY_INFO(info, &cursor) != length) {
      PKCS8_PRIV_KEY_INFO_free(info);
      throw std::runtime_error("could not encode the DPoP key");
    }
    PKCS8_PRIV_KEY_INFO_free(info);
    return der;
  }

  [[nodiscard]] std::vector<unsigned char> sign(const std::string& data) const {
    EVP_MD_CTX* context = EVP_MD_CTX_new();
    if (context == nullptr) throw std::runtime_error("could not sign the DPoP proof");
    std::size_t length = 0;
    if (EVP_DigestSignInit(context, nullptr, nullptr, nullptr, key_) <= 0 ||
        EVP_DigestSign(context, nullptr, &length,
                       reinterpret_cast<const unsigned char*>(data.data()), data.size()) <= 0) {
      EVP_MD_CTX_free(context);
      throw std::runtime_error("could not sign the DPoP proof");
    }
    std::vector<unsigned char> signature(length);
    if (EVP_DigestSign(context, signature.data(), &length,
                       reinterpret_cast<const unsigned char*>(data.data()), data.size()) <= 0) {
      EVP_MD_CTX_free(context);
      throw std::runtime_error("could not sign the DPoP proof");
    }
    signature.resize(length);
    EVP_MD_CTX_free(context);
    return signature;
  }

  EVP_PKEY* key_ = nullptr;
  std::string public_;
};

// The server's current nonce, remembered per origin for the life of one session.
class DpopNonces {
 public:
  std::string get(const std::string& url) const {
    const auto origin = origin_of(url);
    if (origin.empty()) return std::string();
    std::lock_guard lock(mutex_);
    const auto found = values_.find(origin);
    return found == values_.end() ? std::string() : found->second;
  }

  void remember(const std::string& url, const std::string& nonce) {
    const auto origin = origin_of(url);
    if (origin.empty()) return;
    std::lock_guard lock(mutex_);
    values_[origin] = nonce;
  }

 private:
  mutable std::mutex mutex_;
  std::map<std::string, std::string> values_;
};

struct ClientAuthMaterial {
  std::string method = "none";
  std::string client_id;
  std::string secret;
};

ClientAuthMaterial resolve_client_auth(const CredentialStore& credentials,
                                       const nlohmann::json& oauth,
                                       const std::string& client_id) {
  ClientAuthMaterial material;
  material.method = text_field(oauth, "clientAuth", "none");
  material.client_id = client_id;
  if (material.method == "none") return material;
  const auto reference = text_field(oauth, "clientSecretRef");
  if (reference.empty()) {
    throw std::runtime_error(
        "this OAuth client authenticates with a secret, but none is stored; edit the MCP "
        "server and enter it again");
  }
  const auto secret = credentials.get(CredentialReference::parse(reference));
  if (secret.size() != 0) {
    material.secret.assign(reinterpret_cast<const char*>(secret.data()),
                           reinterpret_cast<const char*>(secret.data()) + secret.size());
  }
  if (material.secret.empty()) {
    throw std::runtime_error(
        "the stored OAuth client secret is empty; edit the MCP server and enter it again");
  }
  return material;
}

void apply_client_auth(const ClientAuthMaterial& auth, httplib::Params& form,
                       httplib::Headers& headers) {
  if (auth.method == "none") {
    form.emplace("client_id", auth.client_id);
  } else if (auth.method == "client_secret_post") {
    form.emplace("client_id", auth.client_id);
    form.emplace("client_secret", auth.secret);
  } else if (auth.method == "client_secret_basic") {
    headers.emplace("Authorization",
                    "Basic " + base64_standard(auth.client_id + ":" + auth.secret));
  }
}

struct TokenResponse {
  bool has_access_token = false;
  std::string access_token;
  bool has_token_type = false;
  std::string token_type;
  bool has_expires_in = false;
  long long expires_in = 0;
  bool has_refresh_token = false;
  std::string refresh_token;
  bool has_scope = false;
  std::string scope;
  bool has_error = false;
  std::string error;
  bool has_error_description = false;
  std::string error_description;
};

std::optional<TokenResponse> parse_token_response(const std::string& body) {
  const auto parsed = nlohmann::json::parse(body, nullptr, false);
  if (parsed.is_discarded() || !parsed.is_object()) return std::nullopt;
  TokenResponse response;
  const auto read_string = [&](const char* key, bool& has, std::string& out) {
    if (parsed.contains(key) && parsed.at(key).is_string()) {
      has = true;
      out = parsed.at(key).get<std::string>();
    }
  };
  read_string("access_token", response.has_access_token, response.access_token);
  read_string("token_type", response.has_token_type, response.token_type);
  if (parsed.contains("expires_in") && parsed.at("expires_in").is_number_integer()) {
    response.has_expires_in = true;
    response.expires_in = parsed.at("expires_in").get<long long>();
  }
  read_string("refresh_token", response.has_refresh_token, response.refresh_token);
  read_string("scope", response.has_scope, response.scope);
  read_string("error", response.has_error, response.error);
  read_string("error_description", response.has_error_description, response.error_description);
  return response;
}

struct TokenBundle {
  std::string access_token;
  bool has_refresh = false;
  std::string refresh_token;
  bool has_expires = false;
  long long expires_at = 0;
  std::string token_type;
  bool has_scope = false;
  std::string scope;
};

nlohmann::json bundle_to_json(const TokenBundle& bundle) {
  nlohmann::json value;
  value["access_token"] = bundle.access_token;
  if (bundle.has_refresh) value["refresh_token"] = bundle.refresh_token;
  if (bundle.has_expires) value["expires_at"] = bundle.expires_at;
  value["token_type"] = bundle.token_type;
  if (bundle.has_scope) value["scope"] = bundle.scope;
  return value;
}

TokenBundle bundle_from_json(const nlohmann::json& value) {
  TokenBundle bundle;
  bundle.access_token = text_field(value, "access_token");
  const auto refresh = text_field(value, "refresh_token");
  if (!refresh.empty()) {
    bundle.has_refresh = true;
    bundle.refresh_token = refresh;
  }
  if (value.is_object() && value.contains("expires_at") &&
      value.at("expires_at").is_number_integer()) {
    bundle.has_expires = true;
    bundle.expires_at = value.at("expires_at").get<long long>();
  }
  bundle.token_type = text_field(value, "token_type");
  const auto scope = text_field(value, "scope");
  if (!scope.empty()) {
    bundle.has_scope = true;
    bundle.scope = scope;
  }
  return bundle;
}

bool token_is_fresh(const TokenBundle& bundle) {
  if (!bundle.has_expires) return true;
  return bundle.expires_at > static_cast<long long>(unix_now()) + kTokenExpirySkewSeconds;
}

// The token endpoint's `DPoP-Nonce` challenge. When `allow_challenge` is set this
// also reads the `nonce="..."` parameter of a `WWW-Authenticate: DPoP` header,
// which the MCP request path has to honour.
std::optional<std::string> dpop_nonce(const httplib::Headers& headers, bool allow_challenge) {
  std::string value;
  for (const auto& [name, header_value] : headers) {
    if (ascii_lower(name) == "dpop-nonce") {
      value = header_value;
      break;
    }
  }
  value = trim_ascii(value);
  if (value.empty() && allow_challenge) {
    for (const auto& [name, header_value] : headers) {
      if (ascii_lower(name) != "www-authenticate") continue;
      const auto lowered = ascii_lower(trim_ascii(header_value));
      if (lowered.rfind("dpop", 0) != 0) continue;
      const std::string needle = "nonce=";
      std::size_t from = 0;
      while (true) {
        const auto offset = lowered.find(needle, from);
        if (offset == std::string::npos) break;
        from = offset + needle.size();
        if (offset > 0) {
          const auto previous = static_cast<unsigned char>(header_value[offset - 1]);
          if (std::isalnum(previous) != 0 || previous == 0x2d || previous == 0x5f) continue;
        }
        auto rest = trim_ascii(header_value.substr(from));
        if (!rest.empty() && rest.front() == '"') {
          const auto end = rest.find('"', 1);
          if (end != std::string::npos) {
            value = rest.substr(1, end - 1);
            break;
          }
          continue;
        }
        const auto end = rest.find_first_of(", \t");
        value = rest.substr(0, end);
        break;
      }
      if (!value.empty()) break;
    }
  }
  value = trim_ascii(value);
  if (value.empty() || value.size() > 1024 || has_control_characters(value)) {
    return std::nullopt;
  }
  return value;
}

struct TokenAttempt {
  int status = 0;
  bool has_challenge = false;
  std::string challenge;
  std::optional<TokenResponse> parsed;
  std::string body;
};

TokenBundle token_bundle(const TokenResponse& parsed, const std::string& previous_refresh,
                         bool dpop) {
  if (trim_ascii(parsed.access_token).empty()) {
    throw std::runtime_error("OAuth token response has no access token");
  }
  const std::string expected = dpop ? "dpop" : "bearer";
  if (parsed.has_token_type) {
    if (ascii_lower(trim_ascii(parsed.token_type)) != expected) {
      throw std::runtime_error("OAuth token response is not a " + expected +
                               " token (the server said `" + sanitize(parsed.token_type) + "`)");
    }
  } else if (dpop) {
    throw std::runtime_error(
        "OAuth token response is not a DPoP token: the server did not name a token type");
  }
  TokenBundle bundle;
  bundle.access_token = parsed.access_token;
  if (parsed.has_refresh_token) {
    bundle.has_refresh = true;
    bundle.refresh_token = parsed.refresh_token;
  } else if (!previous_refresh.empty()) {
    bundle.has_refresh = true;
    bundle.refresh_token = previous_refresh;
  }
  if (parsed.has_expires_in && parsed.expires_in >= 0) {
    bundle.has_expires = true;
    bundle.expires_at = static_cast<long long>(unix_now()) + parsed.expires_in;
  }
  bundle.token_type = dpop ? "DPoP" : "Bearer";
  if (parsed.has_scope) {
    bundle.has_scope = true;
    bundle.scope = parsed.scope;
  }
  return bundle;
}

TokenAttempt post_token_form(const std::string& endpoint, const ClientAuthMaterial& auth,
                             httplib::Params form, const DpopKey* dpop,
                             const std::string& nonce) {
  std::string valid;
  if (!validate_oauth_url(endpoint, valid)) {
    throw std::runtime_error("the OAuth token endpoint is not a valid endpoint URL");
  }
  httplib::Headers headers = {{"Accept", "application/json"}};
  apply_client_auth(auth, form, headers);
  if (dpop != nullptr) {
    headers.emplace("DPoP", dpop->proof("POST", endpoint, nonce, std::string()));
  }
  const auto response = http_post_form(endpoint, headers, form, kTokenTimeoutSeconds);
  TokenAttempt attempt;
  attempt.status = response.status;
  if (const auto challenge = dpop_nonce(response.headers, false); challenge.has_value()) {
    attempt.has_challenge = true;
    attempt.challenge = *challenge;
  }
  if (response.body.size() > kMaxTokenResponseBytes) {
    throw std::runtime_error("the OAuth token response exceeds the size limit");
  }
  attempt.parsed = parse_token_response(response.body);
  if (!attempt.parsed.has_value()) {
    attempt.body =
        sanitize(response.body.substr(0, std::min<std::size_t>(response.body.size(), 256)));
  }
  return attempt;
}

TokenBundle finish_token_attempt(const TokenAttempt& attempt, const std::string& previous_refresh,
                                 bool dpop) {
  if (attempt.status < 200 || attempt.status >= 300) {
    std::string detail;
    if (attempt.parsed.has_value()) {
      detail = attempt.parsed->has_error_description ? attempt.parsed->error_description
                                                     : attempt.parsed->error;
    }
    if (detail.empty()) detail = "the authorization server rejected the token request";
    throw std::runtime_error("OAuth token request returned HTTP " +
                             std::to_string(attempt.status) + ": " + sanitize(detail));
  }
  if (!attempt.parsed.has_value()) {
    throw std::runtime_error("OAuth token response is invalid: " +
                             (attempt.body.empty() ? std::string("the body is empty")
                                                   : attempt.body));
  }
  return token_bundle(*attempt.parsed, previous_refresh, dpop);
}

TokenBundle request_token(const std::string& endpoint, const ClientAuthMaterial& auth,
                          httplib::Params form, const std::string& previous_refresh,
                          const DpopKey* dpop, DpopNonces& nonces) {
  std::string valid;
  if (!validate_oauth_url(endpoint, valid)) {
    throw std::runtime_error("the OAuth token endpoint is not a valid endpoint URL");
  }
  const auto cached = dpop != nullptr ? nonces.get(endpoint) : std::string();
  const auto first = post_token_form(endpoint, auth, form, dpop, cached);
  if (first.status == 401 && dpop != nullptr && first.has_challenge &&
      cached != first.challenge) {
    nonces.remember(endpoint, first.challenge);
    const auto second = post_token_form(endpoint, auth, form, dpop, first.challenge);
    return finish_token_attempt(second, previous_refresh, true);
  }
  return finish_token_attempt(first, previous_refresh, dpop != nullptr);
}

struct AuthorizationServerMetadata {
  std::string issuer;
  std::string authorization_endpoint;
  std::string token_endpoint;
  std::optional<std::string> registration_endpoint;
  std::optional<std::string> device_authorization_endpoint;
};

nlohmann::json fetch_json(const std::string& url, std::size_t limit, const std::string& label,
                          const httplib::Headers& headers = {}) {
  const auto response = http_get(url, headers, kDiscoveryTimeoutSeconds);
  if (response.status < 200 || response.status >= 300) {
    throw std::runtime_error("OAuth " + label + " returned HTTP " +
                             std::to_string(response.status));
  }
  if (response.body.size() > limit) {
    throw std::runtime_error("OAuth " + label + " exceeds the size limit");
  }
  auto parsed = nlohmann::json::parse(response.body, nullptr, false);
  if (parsed.is_discarded()) throw std::runtime_error("OAuth " + label + " is invalid");
  return parsed;
}

AuthorizationServerMetadata discover(const std::string& issuer) {
  std::string valid;
  if (!validate_oauth_url(issuer, valid)) {
    throw std::runtime_error("the OAuth issuer is not a valid endpoint URL");
  }
  std::string canonical = valid;
  while (!canonical.empty() && canonical.back() == '/') canonical.pop_back();
  ParsedUrl parsed;
  if (!parse_url(canonical, parsed)) {
    throw std::runtime_error("the OAuth issuer is not a valid endpoint URL");
  }
  std::string path = parsed.path;
  while (!path.empty() && path.front() == '/') path.erase(path.begin());
  while (!path.empty() && path.back() == '/') path.pop_back();
  ParsedUrl discovery = parsed;
  discovery.path = path.empty() ? "/.well-known/oauth-authorization-server"
                                : "/.well-known/oauth-authorization-server/" + path;
  const auto metadata = fetch_json(scheme_host_port(discovery) + discovery.path,
                                   kMaxMetadataBytes, "authorization-server metadata");
  if (!metadata.is_object()) {
    throw std::runtime_error("OAuth authorization-server metadata is invalid");
  }
  std::string metadata_issuer = trim_ascii(text_field(metadata, "issuer"));
  while (!metadata_issuer.empty() && metadata_issuer.back() == '/') metadata_issuer.pop_back();
  if (metadata_issuer != canonical) {
    throw std::runtime_error("OAuth metadata issuer does not match the configured issuer");
  }
  const auto advertises = [&](const char* key, const char* value) -> std::optional<bool> {
    if (!metadata.contains(key) || !metadata.at(key).is_array()) return std::nullopt;
    const auto& list = metadata.at(key);
    if (list.empty()) return std::nullopt;
    for (const auto& item : list) {
      if (item.is_string() && item.get<std::string>() == value) return true;
    }
    return false;
  };
  if (const auto code = advertises("response_types_supported", "code"); code.has_value() && !*code) {
    throw std::runtime_error(
        "OAuth authorization server does not advertise the authorization code flow");
  }
  if (const auto s256 = advertises("code_challenge_methods_supported", "S256");
      s256.has_value() && !*s256) {
    throw std::runtime_error("OAuth authorization server does not advertise PKCE S256");
  }
  AuthorizationServerMetadata result;
  result.issuer = canonical;
  std::string endpoint;
  if (!validate_oauth_url(text_field(metadata, "authorization_endpoint"), endpoint)) {
    throw std::runtime_error("the OAuth authorization endpoint is not a valid endpoint URL");
  }
  result.authorization_endpoint = endpoint;
  if (!validate_oauth_url(text_field(metadata, "token_endpoint"), endpoint)) {
    throw std::runtime_error("the OAuth token endpoint is not a valid endpoint URL");
  }
  result.token_endpoint = endpoint;
  const auto registration = text_field(metadata, "registration_endpoint");
  if (!registration.empty()) {
    if (!validate_oauth_url(registration, endpoint)) {
      throw std::runtime_error("the OAuth registration endpoint is not a valid endpoint URL");
    }
    result.registration_endpoint = endpoint;
  }
  const auto device = text_field(metadata, "device_authorization_endpoint");
  if (!device.empty()) {
    if (!validate_oauth_url(device, endpoint)) {
      throw std::runtime_error(
          "the OAuth device authorization endpoint is not a valid endpoint URL");
    }
    result.device_authorization_endpoint = endpoint;
  }
  return result;
}

// Resolves a (possibly relative) URL against a resource URL.
std::string resolve_relative(const ParsedUrl& base, const std::string& raw) {
  if (raw.rfind("http://", 0) == 0 || raw.rfind("https://", 0) == 0) return raw;
  const auto origin = scheme_host_port(base);
  if (!raw.empty() && raw.front() == '/') return origin + raw;
  std::string directory = base.path;
  const auto slash = directory.rfind('/');
  directory = slash == std::string::npos ? "/" : directory.substr(0, slash + 1);
  return origin + directory + raw;
}

std::string resource_metadata_from_headers(const ParsedUrl& resource,
                                           const httplib::Headers& headers) {
  const std::string needle = "resource_metadata=";
  for (const auto& [name, value] : headers) {
    if (ascii_lower(name) != "www-authenticate") continue;
    const auto lowered = ascii_lower(value);
    const auto index = lowered.find(needle);
    if (index == std::string::npos) continue;
    const auto rest = value.substr(index + needle.size());
    std::string raw;
    if (!rest.empty() && rest.front() == '"') {
      const auto end = rest.find('"', 1);
      raw = rest.substr(1, end == std::string::npos ? std::string::npos : end - 1);
    } else {
      const auto end = rest.find_first_of(", \t");
      raw = rest.substr(0, end);
    }
    if (!raw.empty()) return resolve_relative(resource, raw);
  }
  return std::string();
}

std::string protected_resource_metadata_url(const ParsedUrl& resource) {
  std::string path = resource.path;
  while (!path.empty() && path.front() == '/') path.erase(path.begin());
  while (!path.empty() && path.back() == '/') path.pop_back();
  ParsedUrl metadata = resource;
  metadata.path = path.empty() ? "/.well-known/oauth-protected-resource"
                               : "/.well-known/oauth-protected-resource/" + path;
  return scheme_host_port(metadata) + metadata.path;
}

std::string discover_resource_issuer(const std::string& resource) {
  std::string valid;
  if (!validate_oauth_url(resource, valid)) {
    throw std::runtime_error("the MCP endpoint URL is not a valid endpoint URL");
  }
  ParsedUrl parsed;
  if (!parse_url(valid, parsed)) {
    throw std::runtime_error("the MCP endpoint URL is not a valid endpoint URL");
  }
  const auto probe =
      http_get(valid, {{"Accept", "application/json, text/event-stream"}},
               kDiscoveryTimeoutSeconds);
  std::string metadata_url = resource_metadata_from_headers(parsed, probe.headers);
  if (metadata_url.empty()) metadata_url = protected_resource_metadata_url(parsed);
  std::string metadata_valid;
  if (!validate_oauth_url(metadata_url, metadata_valid)) {
    throw std::runtime_error("the OAuth protected-resource metadata URL is invalid");
  }
  const auto metadata =
      fetch_json(metadata_valid, kMaxMetadataBytes, "protected-resource metadata");
  if (!metadata.is_object()) {
    throw std::runtime_error("OAuth protected-resource metadata is invalid");
  }
  auto metadata_resource = trim_ascii(text_field(metadata, "resource"));
  auto trimmed_resource = trim_ascii(valid);
  while (!metadata_resource.empty() && metadata_resource.back() == '/') {
    metadata_resource.pop_back();
  }
  while (!trimmed_resource.empty() && trimmed_resource.back() == '/') {
    trimmed_resource.pop_back();
  }
  if (metadata_resource != trimmed_resource) {
    throw std::runtime_error(
        "OAuth protected-resource metadata does not match the configured MCP endpoint");
  }
  if (metadata.contains("authorization_servers") &&
      metadata.at("authorization_servers").is_array()) {
    for (const auto& issuer : metadata.at("authorization_servers")) {
      if (!issuer.is_string()) continue;
      std::string candidate;
      if (!validate_oauth_url(issuer.get<std::string>(), candidate)) continue;
      while (!candidate.empty() && candidate.back() == '/') candidate.pop_back();
      return candidate;
    }
  }
  throw std::runtime_error(
      "OAuth protected-resource metadata advertises no valid authorization server");
}

std::string register_client(const std::string& endpoint, const std::string& flow,
                            const std::optional<std::string>& redirect_uri,
                            const std::vector<std::string>& scopes) {
  std::string valid;
  if (!validate_oauth_url(endpoint, valid)) {
    throw std::runtime_error("the OAuth registration endpoint is not a valid endpoint URL");
  }
  nlohmann::json request;
  if (flow == "authorization_code") {
    request["grant_types"] = nlohmann::json::array({"authorization_code", "refresh_token"});
    request["response_types"] = nlohmann::json::array({"code"});
    if (redirect_uri.has_value()) {
      request["redirect_uris"] = nlohmann::json::array({*redirect_uri});
    }
  } else {
    request["grant_types"] = nlohmann::json::array({kDeviceCodeGrant, "refresh_token"});
  }
  request["client_name"] = kDynamicClientName;
  request["token_endpoint_auth_method"] = "none";
  if (!scopes.empty()) request["scope"] = join(scopes, " ");
  const auto response = http_post_json(valid, {{"Content-Type", "application/json"}},
                                       request.dump(), kTokenTimeoutSeconds);
  if (response.body.size() > kMaxRegistrationResponseBytes) {
    throw std::runtime_error("the OAuth registration response exceeds the size limit");
  }
  auto parsed = nlohmann::json::parse(response.body, nullptr, false);
  if (parsed.is_discarded() || !parsed.is_object()) {
    throw std::runtime_error("the OAuth dynamic client registration response is invalid");
  }
  if (response.status < 200 || response.status >= 300) {
    const auto detail = text_field(parsed, "error_description");
    const auto error = text_field(parsed, "error");
    throw std::runtime_error(
        "OAuth dynamic client registration returned HTTP " + std::to_string(response.status) +
        ": " +
        sanitize(detail.empty()
                     ? (error.empty()
                            ? std::string("the authorization server rejected dynamic client "
                                          "registration")
                            : error)
                     : detail));
  }
  if (!text_field(parsed, "client_secret").empty()) {
    throw std::runtime_error(
        "OAuth dynamic client registration issued a client secret, but a public native client "
        "was requested");
  }
  if (text_field(parsed, "token_endpoint_auth_method") != "none") {
    throw std::runtime_error(
        "OAuth dynamic client registration did not confirm token_endpoint_auth_method none");
  }
  const auto client_id = trim_ascii(text_field(parsed, "client_id"));
  if (client_id.empty() || client_id.size() > 512 || has_control_characters(client_id)) {
    throw std::runtime_error("OAuth dynamic client registration returned an invalid client id");
  }
  return client_id;
}

struct FlowState {
  DesktopDatabase& database;
  const CredentialStore& credentials;
  std::string server_id;
  std::string resource;
  nlohmann::json oauth;
  std::optional<DpopKey> dpop;
  DpopNonces& nonces;
  AuthorizationServerMetadata metadata;
};

void persist_oauth(FlowState& state) {
  auto row = state.database.get_mcp_server(state.server_id);
  if (!row.has_value()) {
    throw std::runtime_error("could not load MCP server `" + state.server_id + "` for OAuth");
  }
  (*row)["oauth"] = state.oauth;
  state.database.upsert_mcp_server(*row);
}

std::string resolve_client_id(FlowState& state, const std::string& flow,
                              const std::optional<std::string>& redirect_uri) {
  const auto existing = trim_ascii(text_field(state.oauth, "clientId"));
  if (!existing.empty()) return existing;
  if (!state.metadata.registration_endpoint.has_value()) {
    throw std::runtime_error(
        "OAuth client id is empty and the authorization server does not advertise dynamic "
        "client registration");
  }
  const auto client_id = register_client(*state.metadata.registration_endpoint, flow, redirect_uri,
                                         scope_list(state.oauth));
  state.oauth["clientId"] = client_id;
  persist_oauth(state);
  return client_id;
}

nlohmann::json store_connected_bundle(FlowState& state, const TokenBundle& bundle) {
  const auto reference =
      state.credentials
          .set("mcp", state.server_id + "-" + next_id("oauth"),
               Secret{std::string_view(bundle_to_json(bundle).dump())})
          .to_string();
  const auto previous = text_field(state.oauth, "credentialRef");
  state.oauth["issuer"] = state.metadata.issuer;
  state.oauth["tokenEndpoint"] = state.metadata.token_endpoint;
  state.oauth["credentialRef"] = reference;
  auto row = state.database.get_mcp_server(state.server_id);
  if (!row.has_value()) {
    throw std::runtime_error("could not load MCP server `" + state.server_id + "` for OAuth");
  }
  (*row)["oauth"] = state.oauth;
  try {
    state.database.upsert_mcp_server(*row);
  } catch (...) {
    try {
      state.credentials.erase(CredentialReference::parse(reference));
    } catch (...) {
    }
    throw std::runtime_error("could not save the connected OAuth state");
  }
  if (!previous.empty() && previous != reference) {
    try {
      state.credentials.erase(CredentialReference::parse(previous));
    } catch (...) {
    }
  }
  return {{"serverId", state.server_id},
          {"issuer", state.metadata.issuer},
          {"scopes", state.oauth.contains("scopes") ? state.oauth.at("scopes")
                                                    : nlohmann::json::array()},
          {"tokenEndpoint", state.metadata.token_endpoint}};
}

bool is_cancelled(const std::shared_ptr<std::atomic<bool>>& cancel) {
  return cancel != nullptr && cancel->load();
}

bool sleep_or_cancel(std::chrono::steady_clock::duration total,
                     const std::shared_ptr<std::atomic<bool>>& cancel) {
  auto remaining = total;
  const auto slice = std::chrono::milliseconds(100);
  while (remaining > std::chrono::steady_clock::duration::zero()) {
    if (is_cancelled(cancel)) return false;
    const auto step = std::min(
        remaining,
        std::chrono::duration_cast<std::chrono::steady_clock::duration>(slice));
    std::this_thread::sleep_for(step);
    remaining -= step;
  }
  return !is_cancelled(cancel);
}

struct DeviceAuthorization {
  std::string device_code;
  std::string user_code;
  std::string verification_uri;
  std::optional<std::string> verification_uri_complete;
  std::chrono::steady_clock::duration lifetime{};
  std::chrono::steady_clock::duration interval{};
};

std::chrono::steady_clock::duration device_poll_interval(const std::optional<long long>& seconds) {
  const auto raw = seconds.value_or(kDeviceDefaultPollSeconds);
  std::chrono::steady_clock::duration interval = std::chrono::seconds(raw < 0 ? 0 : raw);
  const auto minimum = std::chrono::milliseconds(250);
  const auto maximum = std::chrono::seconds(300);
  if (interval < minimum) interval = minimum;
  if (interval > maximum) interval = maximum;
  return interval;
}

DeviceAuthorization request_device_authorization(const std::string& endpoint,
                                                 const ClientAuthMaterial& auth,
                                                 httplib::Params form) {
  std::string valid;
  if (!validate_oauth_url(endpoint, valid)) {
    throw std::runtime_error(
        "the OAuth device authorization endpoint is not a valid endpoint URL");
  }
  httplib::Headers headers = {{"Accept", "application/json"}};
  apply_client_auth(auth, form, headers);
  const auto response = http_post_form(valid, headers, form, kTokenTimeoutSeconds);
  if (response.body.size() > kMaxTokenResponseBytes) {
    throw std::runtime_error("the OAuth device authorization response exceeds the size limit");
  }
  auto parsed = nlohmann::json::parse(response.body, nullptr, false);
  if (parsed.is_discarded() || !parsed.is_object()) {
    throw std::runtime_error("the OAuth device authorization response is invalid");
  }
  const bool has_error = parsed.contains("error") && parsed.at("error").is_string();
  if (response.status < 200 || response.status >= 300 || has_error) {
    const auto detail = text_field(parsed, "error_description");
    const auto error = text_field(parsed, "error");
    throw std::runtime_error(
        "OAuth device authorization returned HTTP " + std::to_string(response.status) + ": " +
        sanitize(detail.empty()
                     ? (error.empty()
                            ? std::string("the authorization server refused the device "
                                          "authorization request")
                            : error)
                     : detail));
  }
  const auto device_code = trim_ascii(text_field(parsed, "device_code"));
  const auto user_code = text_field(parsed, "user_code");
  if (device_code.empty() || trim_ascii(user_code).empty()) {
    throw std::runtime_error(
        "OAuth device authorization response has no device code or user code");
  }
  if (user_code.size() > 256 || has_control_characters(user_code)) {
    throw std::runtime_error("OAuth device authorization returned an unusable user code");
  }
  std::string verification_uri;
  if (!validate_oauth_url(text_field(parsed, "verification_uri"), verification_uri)) {
    throw std::runtime_error("OAuth device authorization returned an invalid verification URL");
  }
  std::optional<std::string> verification_uri_complete;
  const auto complete = trim_ascii(text_field(parsed, "verification_uri_complete"));
  if (!complete.empty()) {
    ParsedUrl origin;
    if (!parse_url(complete, origin)) {
      throw std::runtime_error("OAuth device authorization returned an invalid verification URL");
    }
    std::string unused;
    if (!validate_oauth_url(origin.scheme + "://" + origin.host +
                                (origin.port.empty() ? "" : ":" + origin.port) + origin.path,
                            unused)) {
      throw std::runtime_error("OAuth device authorization returned an invalid verification URL");
    }
    verification_uri_complete = complete;
  }
  if (!parsed.contains("expires_in") || !parsed.at("expires_in").is_number_integer()) {
    throw std::runtime_error("the OAuth device authorization response is invalid");
  }
  auto lifetime_seconds = parsed.at("expires_in").get<long long>();
  if (lifetime_seconds < 1) lifetime_seconds = 1;
  if (lifetime_seconds > kDeviceMaxLifetimeSeconds) lifetime_seconds = kDeviceMaxLifetimeSeconds;
  std::optional<long long> interval;
  if (parsed.contains("interval") && parsed.at("interval").is_number_integer()) {
    interval = parsed.at("interval").get<long long>();
  }
  DeviceAuthorization device;
  device.device_code = device_code;
  device.user_code = user_code;
  device.verification_uri = verification_uri;
  device.verification_uri_complete = verification_uri_complete;
  device.lifetime = std::chrono::seconds(lifetime_seconds);
  device.interval = device_poll_interval(interval);
  return device;
}

struct DevicePoll {
  enum class Kind { Pending, SlowDown, Granted };
  Kind kind = Kind::Pending;
  TokenBundle bundle;
};

DevicePoll poll_device_token(const std::string& endpoint, const ClientAuthMaterial& auth,
                             httplib::Params form, const DpopKey* dpop, DpopNonces& nonces) {
  std::string valid;
  if (!validate_oauth_url(endpoint, valid)) {
    throw std::runtime_error("the OAuth token endpoint is not a valid endpoint URL");
  }
  const auto nonce = dpop != nullptr ? nonces.get(valid) : std::string();
  auto attempt = post_token_form(valid, auth, form, dpop, nonce);
  if (attempt.status == 401 && dpop != nullptr && attempt.has_challenge &&
      nonce != attempt.challenge) {
    nonces.remember(valid, attempt.challenge);
    attempt = post_token_form(valid, auth, form, dpop, attempt.challenge);
  }
  if (attempt.parsed.has_value() && attempt.parsed->has_error) {
    const auto description = sanitize(attempt.parsed->has_error_description
                                          ? attempt.parsed->error_description
                                          : std::string("no description"));
    const auto& error = attempt.parsed->error;
    DevicePoll poll;
    if (error == "authorization_pending") return poll;
    if (error == "slow_down") {
      poll.kind = DevicePoll::Kind::SlowDown;
      return poll;
    }
    if (error == "access_denied") {
      throw std::runtime_error(
          "OAuth device authorization was denied by the user (access_denied): " + description);
    }
    if (error == "expired_token") {
      throw std::runtime_error("OAuth device code expired before it was approved (expired_token)");
    }
    throw std::runtime_error("OAuth token request was refused (" + sanitize(error) + "): " +
                             description);
  }
  if (attempt.status < 200 || attempt.status >= 300) {
    throw std::runtime_error("OAuth token request returned HTTP " +
                             std::to_string(attempt.status) + " without an error code");
  }
  if (!attempt.parsed.has_value()) {
    throw std::runtime_error("OAuth token response is invalid: " +
                             (attempt.body.empty() ? std::string("the body is empty")
                                                   : attempt.body));
  }
  DevicePoll poll;
  poll.kind = DevicePoll::Kind::Granted;
  poll.bundle = token_bundle(*attempt.parsed, std::string(), dpop != nullptr);
  return poll;
}

TokenBundle device_code_flow(FlowState& state, const OAuthConnectCallbacks& callbacks) {
  if (!state.metadata.device_authorization_endpoint.has_value()) {
    throw std::runtime_error(
        "the authorization server metadata has no device_authorization_endpoint, so this "
        "server cannot do the device code flow; choose the authorization code flow instead");
  }
  const auto client_id = resolve_client_id(state, "device_code", std::nullopt);
  const auto auth = resolve_client_auth(state.credentials, state.oauth, client_id);
  const auto scope = join(scope_list(state.oauth), " ");
  httplib::Params authorization_form;
  authorization_form.emplace("resource", state.resource);
  if (!scope.empty()) authorization_form.emplace("scope", scope);
  const auto device = request_device_authorization(*state.metadata.device_authorization_endpoint,
                                                   auth, authorization_form);
  const auto deadline = std::chrono::steady_clock::now() + device.lifetime;

  nlohmann::json prompt = {
      {"serverId", state.server_id},
      {"userCode", device.user_code},
      {"verificationUri", device.verification_uri},
      {"expiresAt",
       iso8601_from_epoch(unix_now() + static_cast<std::uint64_t>(
                                            std::chrono::duration_cast<std::chrono::seconds>(
                                                device.lifetime)
                                                .count()))}};
  if (device.verification_uri_complete.has_value()) {
    prompt["verificationUriComplete"] = *device.verification_uri_complete;
  }
  if (callbacks.device_code_prompt) callbacks.device_code_prompt(prompt);
  const auto target = device.verification_uri_complete.value_or(device.verification_uri);
  if (callbacks.open_browser && !callbacks.open_browser(target)) {
    throw std::runtime_error("could not open the system browser to complete OAuth");
  }

  httplib::Params poll_form;
  poll_form.emplace("grant_type", kDeviceCodeGrant);
  poll_form.emplace("device_code", device.device_code);
  poll_form.emplace("resource", state.resource);
  if (!scope.empty()) poll_form.emplace("scope", scope);

  auto interval = device.interval;
  const DpopKey* dpop = state.dpop.has_value() ? &*state.dpop : nullptr;
  for (int polls = 0;; ++polls) {
    if (polls >= kMaxDevicePolls) {
      throw std::runtime_error(
          "OAuth device authorization is still pending after the polling budget; start again");
    }
    const auto now = std::chrono::steady_clock::now();
    if (now >= deadline) {
      throw std::runtime_error("OAuth device code expired before it was approved (expired_token)");
    }
    const auto wait = std::min(interval, deadline - now);
    if (!sleep_or_cancel(wait, callbacks.cancel)) {
      throw std::runtime_error(kCancelledFlow);
    }
    const auto polled =
        poll_device_token(state.metadata.token_endpoint, auth, poll_form, dpop, state.nonces);
    if (polled.kind == DevicePoll::Kind::Pending) continue;
    if (polled.kind == DevicePoll::Kind::SlowDown) {
      interval = std::min(
          interval + std::chrono::seconds(5),
          std::chrono::duration_cast<std::chrono::steady_clock::duration>(
              std::chrono::seconds(300)));
      continue;
    }
    if (is_cancelled(callbacks.cancel)) throw std::runtime_error(kCancelledFlow);
    return polled.bundle;
  }
}

struct LoopbackCallback {
  httplib::Server server;
  std::thread thread;
  ~LoopbackCallback() {
    server.stop();
    if (thread.joinable()) thread.join();
  }
};

struct CallbackSlot {
  std::mutex mutex;
  std::condition_variable cv;
  bool ready = false;
  std::string code;
  std::string error;
};

std::string wait_for_callback(CallbackSlot& slot, LoopbackCallback& listener,
                              const std::shared_ptr<std::atomic<bool>>& cancel) {
  std::unique_lock lock(slot.mutex);
  const auto deadline = std::chrono::steady_clock::now() + kCallbackTimeout;
  while (!slot.ready) {
    if (is_cancelled(cancel)) break;
    if (slot.cv.wait_for(lock, std::chrono::milliseconds(100), [&] { return slot.ready; })) {
      break;
    }
    if (std::chrono::steady_clock::now() >= deadline) break;
  }
  const bool ready = slot.ready;
  const std::string code = slot.code;
  const std::string error = slot.error;
  lock.unlock();
  listener.server.stop();
  if (listener.thread.joinable()) listener.thread.join();
  if (is_cancelled(cancel)) throw std::runtime_error(kCancelledFlow);
  if (!ready) throw std::runtime_error("OAuth callback timed out");
  if (!error.empty()) throw std::runtime_error(error);
  return code;
}

TokenBundle authorization_code_flow(FlowState& state, const OAuthConnectCallbacks& callbacks) {
  LoopbackCallback listener;
  CallbackSlot slot;
  const int port = listener.server.bind_to_any_port("127.0.0.1");
  if (port <= 0) throw std::runtime_error("could not bind the OAuth callback listener");
  const auto redirect_uri = "http://127.0.0.1:" + std::to_string(port) + "/callback";
  const auto expected_state = random_base64url(32);
  const auto verifier = random_base64url(64);
  const auto challenge = base64url(sha256_bytes(verifier));
  listener.server.Get(
      "/callback", [&](const httplib::Request& request, httplib::Response& response) {
        std::string code;
        std::string error;
        if (request.get_param_value("state") != expected_state) {
          error = "OAuth callback state did not match";
        } else if (request.has_param("error")) {
          const auto description =
              request.has_param("error_description")
                  ? sanitize(request.get_param_value("error_description"))
                  : std::string("authorization was rejected");
          error = "OAuth authorization failed: " + sanitize(request.get_param_value("error")) +
                  ": " + description;
        } else if (!request.has_param("code") || request.get_param_value("code").empty()) {
          error = "OAuth callback did not contain an authorization code";
        } else {
          code = request.get_param_value("code");
        }
        {
          std::lock_guard lock(slot.mutex);
          if (!slot.ready) {
            slot.ready = true;
            slot.code = code;
            slot.error = error;
            slot.cv.notify_all();
          }
        }
        response.status = error.empty() ? 200 : 400;
        response.set_content(error.empty() ? kCallbackSuccessHtml : kCallbackFailureHtml,
                             "text/html; charset=utf-8");
        listener.server.stop();
      });
  listener.thread = std::thread([&listener] { listener.server.listen_after_bind(); });

  const auto client_id = resolve_client_id(state, "authorization_code", redirect_uri);
  const auto auth = resolve_client_auth(state.credentials, state.oauth, client_id);
  const auto scopes = scope_list(state.oauth);
  std::string authorization_url = state.metadata.authorization_endpoint;
  const char separator = authorization_url.find('?') == std::string::npos ? '?' : '&';
  std::string query;
  const auto append = [&](const char* name, const std::string& value) {
    query += query.empty() ? std::string(1, separator) : "&";
    query += name;
    query += '=';
    query += url_encode(value);
  };
  append("response_type", "code");
  append("client_id", client_id);
  append("redirect_uri", redirect_uri);
  if (!scopes.empty()) append("scope", join(scopes, " "));
  append("state", expected_state);
  append("code_challenge", challenge);
  append("code_challenge_method", "S256");
  append("resource", state.resource);
  authorization_url += query;
  if (callbacks.open_browser && !callbacks.open_browser(authorization_url)) {
    throw std::runtime_error("could not open the system browser to complete OAuth");
  }

  const auto code = wait_for_callback(slot, listener, callbacks.cancel);
  httplib::Params form;
  form.emplace("grant_type", "authorization_code");
  form.emplace("code", code);
  form.emplace("redirect_uri", redirect_uri);
  form.emplace("code_verifier", verifier);
  form.emplace("resource", state.resource);
  return request_token(state.metadata.token_endpoint, auth, form, std::string(),
                       state.dpop.has_value() ? &*state.dpop : nullptr, state.nonces);
}

}  // namespace

struct OAuthTokenSource::Impl {
  const CredentialStore* credentials = nullptr;
  nlohmann::json oauth;
  std::string resource;
  std::string token_endpoint;
  ClientAuthMaterial auth;
  std::optional<DpopKey> dpop;
  DpopNonces nonces;
  mutable std::mutex mutex;
  std::optional<TokenBundle> cache;

  std::string token_inner(bool force_refresh);
  TokenBundle refresh_locked(const std::string& refresh_token);
  void persist(const CredentialReference& reference, const TokenBundle& bundle) const;
};

std::string OAuthTokenSource::Impl::token_inner(bool force_refresh) {
  std::lock_guard lock(mutex);
  if (!force_refresh && cache.has_value() && token_is_fresh(*cache)) {
    return cache->access_token;
  }
  const auto reference =
      CredentialReference::parse(text_field(oauth, "credentialRef"));
  const auto secret = credentials->get(reference);
  std::string raw;
  if (secret.size() != 0) {
    raw.assign(reinterpret_cast<const char*>(secret.data()),
               reinterpret_cast<const char*>(secret.data()) + secret.size());
  }
  const auto parsed = nlohmann::json::parse(raw, nullptr, false);
  if (parsed.is_discarded() || !parsed.is_object()) {
    throw std::runtime_error("the OAuth token bundle is invalid");
  }
  auto bundle = bundle_from_json(parsed);
  if (!force_refresh && token_is_fresh(bundle)) {
    cache = bundle;
    return bundle.access_token;
  }
  if (!bundle.has_refresh) {
    throw std::runtime_error(
        "the OAuth access token is no longer valid and no refresh token was issued");
  }
  const auto refreshed = refresh_locked(bundle.refresh_token);
  persist(reference, refreshed);
  cache = refreshed;
  return refreshed.access_token;
}

TokenBundle OAuthTokenSource::Impl::refresh_locked(const std::string& refresh_token) {
  httplib::Params form;
  form.emplace("grant_type", "refresh_token");
  form.emplace("refresh_token", refresh_token);
  form.emplace("resource", resource);
  const auto scope = join(scope_list(oauth), " ");
  if (!scope.empty()) form.emplace("scope", scope);
  auto bundle = request_token(token_endpoint, auth, form, std::string(),
                              dpop.has_value() ? &*dpop : nullptr, nonces);
  if (!bundle.has_refresh) {
    bundle.has_refresh = true;
    bundle.refresh_token = refresh_token;
  }
  return bundle;
}

void OAuthTokenSource::Impl::persist(const CredentialReference& reference,
                                     const TokenBundle& bundle) const {
  credentials->set(reference.service, reference.account,
                   Secret{std::string_view(bundle_to_json(bundle).dump())});
}

nlohmann::json oauth_connect(DesktopDatabase& database, const CredentialStore& credentials,
                             const std::string& server_id,
                             const OAuthConnectCallbacks& callbacks) {
  auto row = database.get_mcp_server(server_id);
  if (!row.has_value()) {
    throw std::runtime_error("could not load MCP server `" + server_id + "` for OAuth");
  }
  if (text_field(*row, "transport") != "http") {
    throw std::runtime_error("OAuth can only be configured for Streamable HTTP MCP servers");
  }
  const auto resource = text_field(*row, "url");
  if (resource.empty()) {
    throw std::runtime_error("the MCP endpoint URL must be saved before connecting OAuth");
  }
  if (!row->contains("oauth") || !row->at("oauth").is_object()) {
    throw std::runtime_error("save OAuth configuration before connecting");
  }
  DpopNonces nonces;
  FlowState state{database,
                  credentials,
                  server_id,
                  resource,
                  row->at("oauth"),
                  std::nullopt,
                  nonces,
                  AuthorizationServerMetadata{}};

  if (bool_field(state.oauth, "dpop", false)) {
    auto reference = text_field(state.oauth, "dpopKeyRef");
    if (reference.empty()) {
      std::string encoded;
      auto key = DpopKey::generate(encoded);
      reference = credentials
                      .set("mcp", server_id + "-" + next_id("dpop"),
                           Secret{std::string_view(encoded)})
                      .to_string();
      state.oauth["dpopKeyRef"] = reference;
      persist_oauth(state);
      state.dpop = std::move(key);
    } else {
      state.dpop = DpopKey::load(credentials, reference);
    }
  }

  std::string issuer = trim_ascii(text_field(state.oauth, "issuer"));
  if (issuer.empty()) issuer = discover_resource_issuer(resource);
  state.metadata = discover(issuer);

  const auto flow = text_field(state.oauth, "flow", "authorization_code");
  const auto bundle = flow == "device_code" ? device_code_flow(state, callbacks)
                                            : authorization_code_flow(state, callbacks);
  return store_connected_bundle(state, bundle);
}

OAuthTokenSource::OAuthTokenSource(const CredentialStore* credentials, nlohmann::json oauth,
                                   std::string resource)
    : impl_(std::make_unique<Impl>()) {
  impl_->credentials = credentials;
  impl_->oauth = std::move(oauth);
  impl_->resource = std::move(resource);
  impl_->token_endpoint = text_field(impl_->oauth, "tokenEndpoint");
  std::string valid;
  if (!validate_oauth_url(impl_->token_endpoint, valid)) {
    throw std::runtime_error("the OAuth token endpoint is not a valid endpoint URL");
  }
  impl_->auth =
      resolve_client_auth(*credentials, impl_->oauth, text_field(impl_->oauth, "clientId"));
  if (bool_field(impl_->oauth, "dpop", false)) {
    const auto reference = text_field(impl_->oauth, "dpopKeyRef");
    if (reference.empty()) {
      throw std::runtime_error("the DPoP key is missing from the stored OAuth configuration");
    }
    impl_->dpop = DpopKey::load(*credentials, reference);
  }
}

bool OAuthTokenSource::uses_dpop() const { return impl_->dpop.has_value(); }

void OAuthTokenSource::remember_nonce(const std::string& url, const std::string& nonce) {
  impl_->nonces.remember(url, nonce);
}

OAuthAuthorization OAuthTokenSource::authorize(const std::string& method, const std::string& url,
                                               const std::string& nonce, bool force_refresh) {
  const auto token = impl_->token_inner(force_refresh);
  OAuthAuthorization authorization;
  if (!impl_->dpop.has_value()) {
    authorization.scheme = "Bearer";
    authorization.token = token;
    return authorization;
  }
  std::string effective;
  if (!nonce.empty()) {
    impl_->nonces.remember(url, nonce);
    effective = nonce;
  } else {
    effective = impl_->nonces.get(url);
  }
  authorization.scheme = "DPoP";
  authorization.token = token;
  authorization.proof = impl_->dpop->proof(method, url, effective, token);
  return authorization;
}

#else  // !CPPHTTPLIB_OPENSSL_SUPPORT

struct OAuthTokenSource::Impl {
  int unused = 0;
};

nlohmann::json oauth_connect(DesktopDatabase&, const CredentialStore&, const std::string&,
                             const OAuthConnectCallbacks&) {
  throw std::runtime_error(
      "OAuth requires the OpenSSL-enabled desktop host build (YUKINAL_BUILD_SSH_SERVICE)");
}

OAuthTokenSource::OAuthTokenSource(const CredentialStore*, nlohmann::json, std::string)
    : impl_(std::make_unique<Impl>()) {}

bool OAuthTokenSource::uses_dpop() const { return false; }

void OAuthTokenSource::remember_nonce(const std::string&, const std::string&) {}

OAuthAuthorization OAuthTokenSource::authorize(const std::string&, const std::string&,
                                               const std::string&, bool) {
  throw std::runtime_error(
      "OAuth requires the OpenSSL-enabled desktop host build (YUKINAL_BUILD_SSH_SERVICE)");
}

#endif  // CPPHTTPLIB_OPENSSL_SUPPORT

OAuthTokenSource::~OAuthTokenSource() = default;
OAuthTokenSource::OAuthTokenSource(OAuthTokenSource&&) noexcept = default;
OAuthTokenSource& OAuthTokenSource::operator=(OAuthTokenSource&&) noexcept = default;

}  // namespace yukinal::desktop_host

#pragma once

#include <atomic>
#include <functional>
#include <memory>
#include <nlohmann/json.hpp>
#include <string>

namespace yukinal::desktop_host {

class DesktopDatabase;
class CredentialStore;

// What the OAuth authorization flow needs from the desktop host: a way to open
// the system browser, a way to surface the RFC 8628 device-code prompt, and a
// cancellation token. Everything else (discovery, registration, the token
// exchange, credential persistence) lives in the engine.
struct OAuthConnectCallbacks {
  std::function<bool(const std::string& url)> open_browser;
  std::function<void(const nlohmann::json& prompt)> device_code_prompt;
  std::shared_ptr<std::atomic<bool>> cancel;
};

// Runs the OAuth authorization configured on one HTTP MCP server and stores the
// resulting token bundle in the OS credential store. Blocks until the flow ends
// (the browser redirect waits for the loopback callback; the device-code flow
// polls until the user approves, times out, or `cancel` is set). Throws
// std::runtime_error with a user-facing message on failure.
[[nodiscard]] nlohmann::json oauth_connect(DesktopDatabase& database,
                                           const CredentialStore& credentials,
                                           const std::string& server_id,
                                           const OAuthConnectCallbacks& callbacks);

// The token material one MCP request needs. `proof` is non-empty exactly when
// the scheme is `DPoP`.
struct OAuthAuthorization {
  std::string scheme;
  std::string token;
  std::string proof;
};

// A dynamic OAuth token source for one connected MCP server. It reads the stored
// bundle, refreshes it when it is close to expiring, and signs a per-request
// DPoP proof when the server asked for sender-constrained tokens. The instance
// caches the current bundle in memory and is not shared across sessions.
class OAuthTokenSource {
 public:
  OAuthTokenSource(const CredentialStore* credentials, nlohmann::json oauth, std::string resource);
  ~OAuthTokenSource();
  OAuthTokenSource(const OAuthTokenSource&) = delete;
  OAuthTokenSource& operator=(const OAuthTokenSource&) = delete;
  OAuthTokenSource(OAuthTokenSource&&) noexcept;
  OAuthTokenSource& operator=(OAuthTokenSource&&) noexcept;

  // Resolves a usable access token (refreshing if needed) and, for DPoP, signs a
  // proof bound to `method` and `url`. `nonce` is the value from a `DPoP-Nonce`
  // challenge on a previous attempt, when this is a retry.
  [[nodiscard]] OAuthAuthorization authorize(const std::string& method,
                                             const std::string& url,
                                             const std::string& nonce,
                                             bool force_refresh);
  void remember_nonce(const std::string& url, const std::string& nonce);
  [[nodiscard]] bool uses_dpop() const;

 private:
  struct Impl;
  std::unique_ptr<Impl> impl_;
};

}  // namespace yukinal::desktop_host

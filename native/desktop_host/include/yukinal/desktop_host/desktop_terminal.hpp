#pragma once

#include <chrono>
#include <cstddef>
#include <cstdint>
#include <functional>
#include <iterator>
#include <map>
#include <memory>
#include <nlohmann/json.hpp>
#include <optional>
#include <string>
#include <vector>

namespace yukinal::desktop_host {

class DesktopDatabase;
class CredentialStore;

// Pending keyboard-interactive (RFC 4256) challenges raised by the PTY workers.
//
// The SSH service asks for credentials *mid-handshake* by emitting an
// `auth_prompt` frame, and it accepts exactly one `auth_response` back. Two
// shapes matter and both are the service's, not this host's invention:
//
//  * the answer must carry the same number of strings as the challenge had
//    prompts — the service marks the whole session `invalid` on a mismatch, so
//    a wrong count must be refused here instead of being forwarded;
//  * a challenge is only answerable once, and only while it is live.
//
// Kept out of `TerminalManager::Impl` so the state machine can be exercised
// without spawning a process: the terminal tests drive this class directly.
class AuthChallenges {
 public:
  struct Entry {
    std::string session_id;
    std::size_t expected = 0;
    std::chrono::steady_clock::time_point expires_at;
  };

  // How long a challenge stays answerable. A prompt the user never answers must
  // not accumulate forever, and the service gives up on its own side anyway.
  static constexpr std::chrono::seconds kLifetime{180};

  // Registers a challenge for `session_id` and returns the id the UI answers
  // with. Ids are opaque and unique for the lifetime of this object.
  std::string begin(const std::string& session_id, std::size_t expected,
                    std::chrono::steady_clock::time_point now) {
    sweep(now);
    const auto auth_id = "auth-" + std::to_string(++sequence_);
    entries_[auth_id] = Entry{session_id, expected, now + kLifetime};
    return auth_id;
  }

  // Hands the challenge over for answering. On success the challenge is
  // consumed, so a second answer is refused. A wrong response count and an
  // expired challenge are refused *without* consuming it where that is still
  // useful (a user may retype a code), but both report an explicit reason.
  std::optional<Entry> take(const std::string& auth_id, std::size_t responses,
                            std::chrono::steady_clock::time_point now,
                            std::string& error) {
    const auto found = entries_.find(auth_id);
    if (found == entries_.end()) {
      error = "no authentication challenge is waiting for that id";
      return std::nullopt;
    }
    if (now >= found->second.expires_at) {
      entries_.erase(found);
      error = "the authentication challenge has expired";
      return std::nullopt;
    }
    if (responses != found->second.expected) {
      error = "the authentication challenge expects " +
              std::to_string(found->second.expected) + " response(s), not " +
              std::to_string(responses);
      return std::nullopt;
    }
    const auto entry = found->second;
    entries_.erase(found);
    return entry;
  }

  // Withdraws a challenge, reporting the session it belonged to. Returns false
  // when no such challenge is pending.
  bool cancel(const std::string& auth_id, std::string& session_id) {
    const auto found = entries_.find(auth_id);
    if (found == entries_.end()) return false;
    session_id = found->second.session_id;
    entries_.erase(found);
    return true;
  }

  // Drops expired challenges and every challenge of a session that has ended.
  void sweep(std::chrono::steady_clock::time_point now) {
    for (auto it = entries_.begin(); it != entries_.end();) {
      it = now >= it->second.expires_at ? entries_.erase(it) : std::next(it);
    }
  }

  // Drops the challenges belonging to one session (it closed or failed).
  void forget_session(const std::string& session_id) {
    for (auto it = entries_.begin(); it != entries_.end();) {
      it = it->second.session_id == session_id ? entries_.erase(it) : std::next(it);
    }
  }

  [[nodiscard]] std::size_t size() const { return entries_.size(); }

 private:
  std::map<std::string, Entry> entries_;
  std::uint64_t sequence_ = 0;
};

// Owns the SSH PTY children backing the UI terminal. Each session spawns the
// C++ SSH service in `--terminal` mode with a one-target config (metadata plus
// environment-variable names; secret values go to the child environment), then
// relays the JSON-lines frames as desktop events.
class TerminalManager {
 public:
  using EventSink = std::function<void(const std::string&, const nlohmann::json&)>;

  TerminalManager(DesktopDatabase* database, const CredentialStore* credentials);
  ~TerminalManager();
  TerminalManager(const TerminalManager&) = delete;
  TerminalManager& operator=(const TerminalManager&) = delete;

  void set_event_sink(EventSink sink);

  // Opens one PTY and returns its opaque session id. Throws std::runtime_error
  // with a stable message when the target or process could not be started.
  [[nodiscard]] std::string open(const std::string& server_id, int cols, int rows);
  [[nodiscard]] bool write(const std::string& session_id, const std::string& data);
  [[nodiscard]] bool resize(const std::string& session_id, int cols, int rows);
  [[nodiscard]] bool close(const std::string& session_id);

  // Answers a keyboard-interactive challenge: the responses are encoded as the
  // service's `auth_response` frame and written to that session's stdin. Returns
  // false and fills `error` for an unknown, expired or already-answered
  // challenge, and for a response count the challenge did not ask for — the
  // service kills the session on a count mismatch, so it is never forwarded.
  [[nodiscard]] bool respond_auth(const std::string& auth_id,
                                  const std::vector<std::string>& responses, std::string& error);

  // Withdraws a challenge and ends the session that raised it. A cancelled
  // challenge must not leave the terminal hanging in the authentication phase.
  [[nodiscard]] bool cancel_auth(const std::string& auth_id, std::string& error);

 private:
  struct Impl;
  std::unique_ptr<Impl> impl_;
};

}  // namespace yukinal::desktop_host

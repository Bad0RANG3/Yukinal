#pragma once

#include <functional>
#include <memory>
#include <nlohmann/json.hpp>
#include <string>

namespace yukinal::desktop_host {

class DesktopDatabase;
class CredentialStore;

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

 private:
  struct Impl;
  std::unique_ptr<Impl> impl_;
};

}  // namespace yukinal::desktop_host

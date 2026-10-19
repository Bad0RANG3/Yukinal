#pragma once

// Parsing and serialization for the SSH service's terminal `closed` frame.
//
// The host reports the *remote* shell's exit status. It must never report the
// status of the `yukinal-ssh-service` wrapper process: the wrapper exits with
// its own status (in practice 0) even when the remote command failed, which
// turns a failure into a UI success. The service writes `closed` before it
// exits, so that frame is authoritative:
//
//   {"type":"closed","exit_code":7}     remote status 7
//   {"type":"closed","exit_code":null}  remote status unknown
//
// No frame at all also means unknown and is serialized as null, which the
// shared IPC schema accepts (`exitCode: number | null`).
//
// This header is private to the desktop host implementation (not installed
// public API) but is included by the host tests so the frame contract can be
// exercised without spawning a PTY.

#include <nlohmann/json.hpp>

#include <cstdint>
#include <limits>
#include <optional>
#include <string>

namespace yukinal::desktop_host::terminal_detail {

struct RemoteExit {
  // Whether a `closed` frame arrived. It distinguishes "frame carried null"
  // from "no frame", although both serialize as null.
  bool received = false;
  std::optional<int> value;
};

// Applies one `closed` frame body. Returns std::nullopt when the frame is
// malformed (missing or wrongly typed `exit_code`) and the caller should keep
// waiting. A non-negative integer is stored; a value outside the IPC schema's
// non-negative int range, or an explicit null, yields `received == true` with
// no value, so the caller reports unknown rather than a fabricated status.
inline std::optional<RemoteExit> parse_closed_frame(const nlohmann::json& frame,
                                                    std::string& warning) {
  if (!frame.is_object()) return std::nullopt;
  const auto found = frame.find("exit_code");
  if (found == frame.end()) return std::nullopt;
  if (found->is_null()) return RemoteExit{true, std::nullopt};
  std::uint64_t reported = 0;
  if (found->is_number_unsigned()) {
    reported = found->get<std::uint64_t>();
  } else if (found->is_number_integer()) {
    const auto signed_value = found->get<std::int64_t>();
    if (signed_value < 0) {
      warning = "terminal closed with an out-of-range exit code";
      return RemoteExit{true, std::nullopt};
    }
    reported = static_cast<std::uint64_t>(signed_value);
  } else {
    return std::nullopt;
  }
  if (reported > static_cast<std::uint64_t>(std::numeric_limits<int>::max())) {
    warning = "terminal closed with an out-of-range exit code";
    return RemoteExit{true, std::nullopt};
  }
  return RemoteExit{true, static_cast<int>(reported)};
}

// The `terminal.closed` event payload. The status is null when nothing
// authoritative arrived.
inline nlohmann::json closed_event_payload(const std::string& session_id,
                                           const RemoteExit& remote_exit) {
  nlohmann::json exit_code = nullptr;
  if (remote_exit.received && remote_exit.value.has_value()) exit_code = *remote_exit.value;
  return {{"terminalSessionId", session_id}, {"exitCode", std::move(exit_code)}};
}

// The state machine behind one session's `closed` frame handling, split out of
// `TerminalManager::Impl` so the host can be tested through the real frame path
// without spawning a PTY process. `TerminalManager` owns one per session under
// that session's mutex and calls `observe_frame` from its frame handler; tests
// call `observe_line`, which performs the same JSON parse and `type` dispatch
// on a synthetic protocol line.
class TerminalExitTracker {
 public:
  // Applies an already-parsed frame. Returns true when the frame was a valid
  // `closed` frame that updated the remote exit state; `warning` is filled for
  // a frame that was accepted but whose status cannot be represented.
  bool observe_frame(const nlohmann::json& frame, std::string& warning) {
    if (!frame.is_object()) return false;
    if (frame.value("type", std::string()) != "closed") return false;
    const auto parsed = parse_closed_frame(frame, warning);
    if (!parsed.has_value()) return false;
    state_ = *parsed;
    return true;
  }

  // Convenience for tests and callers holding a raw protocol line.
  bool observe_line(const std::string& line, std::string& warning) {
    const auto frame = nlohmann::json::parse(line, nullptr, false);
    if (frame.is_discarded()) return false;
    return observe_frame(frame, warning);
  }

  [[nodiscard]] const RemoteExit& state() const noexcept { return state_; }
  [[nodiscard]] nlohmann::json payload(const std::string& session_id) const {
    return closed_event_payload(session_id, state_);
  }

 private:
  RemoteExit state_;
};

}  // namespace yukinal::desktop_host::terminal_detail

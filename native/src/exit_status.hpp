#pragma once

namespace yukinal::ssh_service::detail {

/// Interpretation of libssh2's channel exit-status state.
struct ExitStatus {
  /// True only when the peer actually sent an SSH "exit-status" request.
  bool received = false;
  /// The received status. Only meaningful when `received` is true.
  int value = 0;
};

/// Map libssh2's (`libssh2_channel_has_exit_status`, status) pair onto the
/// service's exit-status contract.
///
/// `libssh2_channel_get_exit_status` alone cannot report the absence of the
/// "exit-status" request: libssh2 seeds the channel's status field to zero and
/// only overwrites it when the request arrives, so a server that closes the
/// channel without one is indistinguishable from a clean `exit 0`. The
/// companion `libssh2_channel_has_exit_status` exposes the `exit_status_received`
/// flag and is therefore the only reliable signal.
///
/// The status is passed through verbatim when it was received. RFC 4254 carries
/// it as a `uint32` and libssh2 narrows that to a signed `int` without a range
/// check, so a status such as 0x80000000 legitimately arrives negative: the
/// remote result is known, only its reading under the 8-bit shell convention is
/// unusual. Callers that require the POSIX 0..255 range validate it themselves.
inline ExitStatus interpret_exit_status(int has_exit_status, int status) {
  if (has_exit_status == 0) return ExitStatus{};
  return ExitStatus{true, status};
}

}  // namespace yukinal::ssh_service::detail

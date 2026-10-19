#pragma once

#include <algorithm>
#include <chrono>

namespace yukinal::ssh_service::detail {

// One fresh SFTP metadata connection has a single absolute deadline. Phase
// limits are ceilings within it, never additional allowances or idle timers.
class SftpMetadataDeadline {
 public:
  using Clock = std::chrono::steady_clock;
  enum class Phase { none, host_key, connection, handshake, authentication, startup, operation };

  SftpMetadataDeadline(Clock::time_point started, std::chrono::seconds total)
      : overall_(started + total), phase_deadline_(overall_) {}

  Clock::time_point enter(Phase phase, Clock::time_point now = Clock::now()) {
    // A repeated wait within a phase must not restart that phase's allowance.
    if (phase != phase_) {
      phase_ = phase;
      phase_deadline_ = (std::min)(overall_, now + limit(phase));
    }
    return phase_deadline_;
  }

  Clock::time_point overall() const { return overall_; }
  Phase phase() const { return phase_; }

  static constexpr std::chrono::seconds limit(Phase phase) {
    using namespace std::chrono_literals;
    switch (phase) {
      case Phase::connection: return 10s;  // DNS, TCP and configured proxy.
      case Phase::handshake: return 10s;
      case Phase::authentication: return 15s;
      case Phase::startup: return 5s;      // Channel, subsystem and SFTP version.
      case Phase::operation: return 10s;   // Entire listing/stat/publication.
      default: return 30s;                // Local host-key verification.
    }
  }

  const char* phase_label() const {
    switch (phase_) {
      case Phase::host_key: return "主机身份校验";
      case Phase::connection: return "连接";
      case Phase::handshake: return "SSH 握手";
      case Phase::authentication: return "SSH 认证";
      case Phase::startup: return "SFTP 初始化";
      case Phase::operation: return "文件操作";
      default: return "准备";
    }
  }

 private:
  Clock::time_point overall_;
  Clock::time_point phase_deadline_;
  Phase phase_ = Phase::none;
};

}  // namespace yukinal::ssh_service::detail

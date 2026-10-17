#pragma once

#include <cstdint>
#include <functional>
#include <memory>
#include <nlohmann/json.hpp>
#include <optional>
#include <stdexcept>
#include <string>
#include <vector>

namespace yukinal::desktop_host {

// Host-owned, one-time tickets for exact Python-to-host action approvals.
//
// The sidecar emits `agent.waiting_approval` as an observation, but only the
// host can mint a ticket: this ledger records the event, moves it to an approved
// state only after the host learns the sidecar accepted the user's decision, and
// consumes it once against the full host tool request before any remote effect.
//
// A recovered sidecar re-emits the same pending approval id with a fresh
// deadline; that replay only refreshes the existing ticket. Everything else
// fails closed.
class HostApprovalLedger {
 public:
  // A decision accepted just enough to be forwarded, but not yet approved. The
  // ticket only becomes executable when the sidecar confirms `accepted`.
  struct ResponseToken {
    std::string approval_id;
    bool approve_once = false;
  };

  HostApprovalLedger();
  ~HostApprovalLedger();
  HostApprovalLedger(const HostApprovalLedger&) = delete;
  HostApprovalLedger& operator=(const HostApprovalLedger&) = delete;

  // Capture only well-formed host action approvals, retaining a host-computed
  // fingerprint of the exact host input instead of the command text. Throws
  // std::runtime_error with a user-facing message when the event is malformed or
  // unsupported; the caller logs and continues.
  bool record_waiting_event(const nlohmann::json& params, std::int64_t now);

  // Move a known host action ticket to waiting-for-ack. Returns std::nullopt when
  // the approval belongs to another tool, which leaves it on its current round
  // trip. Throws std::runtime_error for an expired, already-answered, mismatched
  // or per-session decision.
  std::optional<ResponseToken> begin_response(const std::string& approval_id,
                                              const std::string& run_id,
                                              const std::string& decision,
                                              std::int64_t now);

  // Complete the response. A ticket becomes executable only when the sidecar
  // accepted an approve-once decision; failed or rejected responses remove it.
  void finish_response(const ResponseToken& token, bool accepted, std::int64_t now);

  enum class TicketError {
    Missing,
    Expired,
    Mismatch,
    NotApproved,
  };

  // Fields a host tool request supplies for comparison with the approval ticket.
  struct ExecutionBinding {
    std::string approval_id;
    std::optional<std::string> run_id;
    std::string trace_id;
    std::string call_id;
    std::string tool_name;
    std::string input_fingerprint;
    std::string target_host;
    std::optional<std::string> server_id;
    std::optional<std::string> workspace_id;
    std::string environment;
    std::optional<std::string> task_id;
    std::optional<std::string> plan_id;
    std::optional<std::string> plan_step_id;
    std::optional<std::vector<std::string>> evidence_ids;
  };

  // Consume the ticket exactly once. A request racing the approval RPC waits for
  // its host-side acknowledgement; a mismatch burns the approved ticket.
  // std::nullopt means the ticket was consumed and matched.
  std::optional<TicketError> consume(const ExecutionBinding& request,
                                     const std::function<std::int64_t()>& now);

 private:
  struct Impl;
  std::unique_ptr<Impl> impl_;
};

// The four built-in SSH actions that require a host-issued approval ticket.
[[nodiscard]] bool is_host_approval_tool_name(const std::string& tool_name);

}  // namespace yukinal::desktop_host

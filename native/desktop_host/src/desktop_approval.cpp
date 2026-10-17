#include "yukinal/desktop_host/desktop_approval.hpp"

#include "yukinal/desktop_host/content_revision.hpp"

#include <algorithm>
#include <condition_variable>
#include <ctime>
#include <iomanip>
#include <map>
#include <mutex>
#include <sstream>
#include <unordered_map>

namespace yukinal::desktop_host {
namespace {

constexpr std::size_t kMaxApprovalIdChars = 256;
constexpr std::size_t kMaxRunIdChars = 256;
constexpr std::size_t kMaxTraceIdChars = 256;
constexpr std::size_t kMaxCallIdChars = 256;
constexpr std::size_t kMaxServerIdChars = 256;
constexpr std::size_t kMaxEnvironmentChars = 64;
constexpr std::size_t kMaxTickets = 256;
constexpr std::int64_t kMaxApprovalTtlSeconds = 180;

constexpr const char* kToolServerExec = "server.exec";
constexpr const char* kToolDockerRestart = "docker.restart";
constexpr const char* kToolSystemdRestart = "systemd.restart";
constexpr const char* kToolPackageInstall = "package.install";

bool is_sha256_hex(const std::string& value) {
  if (value.size() != 64) return false;
  return std::all_of(value.begin(), value.end(), [](unsigned char byte) {
    return (byte >= '0' && byte <= '9') || (byte >= 'a' && byte <= 'f') ||
           (byte >= 'A' && byte <= 'F');
  });
}

std::optional<std::int64_t> parse_iso8601_utc_seconds(const std::string& value) {
  if (value.size() < 19) return std::nullopt;
  std::tm utc{};
  std::istringstream stream(value);
  stream >> std::get_time(&utc, "%Y-%m-%dT%H:%M:%S");
  if (stream.fail()) return std::nullopt;
#ifdef _WIN32
  const auto seconds = _mkgmtime(&utc);
#else
  const auto seconds = timegm(&utc);
#endif
  if (seconds == static_cast<std::time_t>(-1)) return std::nullopt;
  std::int64_t result = static_cast<std::int64_t>(seconds);
  if (value.size() > 19 && (value[19] == '+' || value[19] == '-')) {
    if (value.size() < 25) return std::nullopt;
    const int hours = std::stoi(value.substr(20, 2));
    const int minutes = std::stoi(value.substr(23, 2));
    const std::int64_t offset = static_cast<std::int64_t>(hours) * 3600 + minutes * 60;
    result += value[19] == '+' ? -offset : offset;
  }
  return result;
}

bool json_string_field(const nlohmann::json& value, const char* key, std::string& out) {
  if (!value.is_object() || !value.contains(key) || !value.at(key).is_string()) return false;
  out = value.at(key).get<std::string>();
  return true;
}

bool json_optional_string_field(const nlohmann::json& value, const char* key,
                                std::optional<std::string>& out) {
  if (!value.is_object()) return false;
  const auto found = value.find(key);
  if (found == value.end() || found->is_null()) {
    out.reset();
    return true;
  }
  if (!found->is_string()) return false;
  out = found->get<std::string>();
  return true;
}

bool json_optional_string_list_field(const nlohmann::json& value, const char* key,
                                     std::optional<std::vector<std::string>>& out) {
  if (!value.is_object()) return false;
  const auto found = value.find(key);
  if (found == value.end() || found->is_null()) {
    out.reset();
    return true;
  }
  if (!found->is_array()) return false;
  std::vector<std::string> items;
  for (const auto& item : *found) {
    if (!item.is_string()) return false;
    items.push_back(item.get<std::string>());
  }
  out = std::move(items);
  return true;
}

}  // namespace

bool is_host_approval_tool_name(const std::string& tool_name) {
  return tool_name == kToolServerExec || tool_name == kToolDockerRestart ||
         tool_name == kToolSystemdRestart || tool_name == kToolPackageInstall;
}

struct HostApprovalLedger::Impl {
  enum class Phase { Pending, Responding, Approved };

  struct Binding {
    std::string approval_id;
    std::string run_id;
    std::string trace_id;
    std::string call_id;
    std::string tool_name;
    std::string input_fingerprint;
    std::string target_host;
    std::string server_id;
    std::optional<std::string> workspace_id;
    std::string environment;
    std::string task_id;
    std::string plan_id;
    std::string plan_step_id;
    std::optional<std::vector<std::string>> evidence_ids;
    std::int64_t expires_at = 0;

    bool same_request(const Binding& other) const {
      return approval_id == other.approval_id && run_id == other.run_id &&
             trace_id == other.trace_id && call_id == other.call_id &&
             tool_name == other.tool_name &&
             input_fingerprint == other.input_fingerprint &&
             target_host == other.target_host && server_id == other.server_id &&
             workspace_id == other.workspace_id && environment == other.environment &&
             task_id == other.task_id && plan_id == other.plan_id &&
             plan_step_id == other.plan_step_id && evidence_ids == other.evidence_ids;
    }

    bool matches(const ExecutionBinding& request) const {
      return approval_id == request.approval_id && request.run_id == run_id &&
             trace_id == request.trace_id && call_id == request.call_id &&
             tool_name == request.tool_name &&
             is_host_approval_tool_name(request.tool_name) &&
             input_fingerprint == request.input_fingerprint &&
             target_host == request.target_host && request.server_id == server_id &&
             workspace_id == request.workspace_id && environment == request.environment &&
             request.task_id == task_id && request.plan_id == plan_id &&
             request.plan_step_id == plan_step_id && evidence_ids == request.evidence_ids;
    }
  };

  struct Entry {
    Binding binding;
    Phase phase = Phase::Pending;
  };

  std::mutex mutex;
  std::condition_variable changed;
  std::unordered_map<std::string, Entry> entries;
};

HostApprovalLedger::HostApprovalLedger() : impl_(std::make_unique<Impl>()) {}
HostApprovalLedger::~HostApprovalLedger() = default;

bool HostApprovalLedger::record_waiting_event(const nlohmann::json& params,
                                              std::int64_t now) {
  if (!params.is_object() || !params.contains("approval") ||
      !params.at("approval").is_object()) {
    throw std::runtime_error("approval event is missing host binding fields");
  }
  const auto& approval = params.at("approval");

  std::string event_type;
  std::string run_id;
  std::string approval_id;
  std::string approval_run_id;
  std::string trace_id;
  std::string call_id;
  std::string tool_name;
  std::string input_fingerprint;
  std::string expires_at;
  if (!json_string_field(params, "type", event_type) ||
      !json_string_field(params, "runId", run_id) ||
      !json_string_field(approval, "approvalId", approval_id) ||
      !json_string_field(approval, "runId", approval_run_id) ||
      !json_string_field(approval, "traceId", trace_id) ||
      !json_string_field(approval, "callId", call_id) ||
      !json_string_field(approval, "toolName", tool_name) ||
      !json_string_field(approval, "inputFingerprint", input_fingerprint) ||
      !json_string_field(approval, "expiresAt", expires_at) ||
      !approval.contains("target") || !approval.at("target").is_object()) {
    throw std::runtime_error("approval event is missing host binding fields");
  }

  const auto& target = approval.at("target");
  std::string target_host;
  std::string environment;
  std::optional<std::string> server_id;
  std::optional<std::string> workspace_id;
  if (!json_string_field(target, "host", target_host) ||
      !json_string_field(target, "environment", environment) ||
      !json_optional_string_field(target, "serverId", server_id) ||
      !json_optional_string_field(target, "workspaceId", workspace_id)) {
    throw std::runtime_error("approval event is missing host binding fields");
  }

  if (event_type != "agent.waiting_approval" || !is_host_approval_tool_name(tool_name) ||
      approval_run_id != run_id || run_id.empty() || run_id.size() > kMaxRunIdChars ||
      approval_id.empty() || approval_id.size() > kMaxApprovalIdChars ||
      trace_id.empty() || trace_id.size() > kMaxTraceIdChars || call_id.empty() ||
      call_id.size() > kMaxCallIdChars || !is_sha256_hex(input_fingerprint) ||
      target_host != "remote" ||
      !server_id.has_value() || !server_id->starts_with("srv_") ||
      server_id->size() > kMaxServerIdChars || environment.empty() ||
      environment.size() > kMaxEnvironmentChars) {
    throw std::runtime_error(
        "approval event does not describe a supported host action binding");
  }

  std::optional<std::string> task_id;
  if (!json_optional_string_field(params, "taskId", task_id) || !task_id.has_value() ||
      task_id->empty()) {
    throw std::runtime_error("host action approval requires a durable task");
  }
  std::optional<std::string> plan_id;
  std::optional<std::string> plan_step_id;
  if (!json_optional_string_field(approval, "planId", plan_id) || !plan_id.has_value() ||
      plan_id->empty()) {
    throw std::runtime_error("host action approval requires a current plan");
  }
  if (!json_optional_string_field(approval, "planStepId", plan_step_id) ||
      !plan_step_id.has_value() || plan_step_id->empty()) {
    throw std::runtime_error("host action approval requires a current plan step");
  }

  const auto expires_at_epoch = parse_iso8601_utc_seconds(expires_at);
  if (!expires_at_epoch.has_value()) {
    throw std::runtime_error("host action approval expiry is invalid");
  }
  if (*expires_at_epoch <= now ||
      *expires_at_epoch - now > kMaxApprovalTtlSeconds) {
    throw std::runtime_error(
        "host action approval is expired or exceeds the host TTL limit");
  }

  // The sidecar fingerprint includes Python authorization context. Tickets bind
  // to the exact host-tool input instead, with the canonical JSON fingerprint the
  // host recomputes before dispatch.
  const auto host_input = approval.find("hostInput");
  if (host_input == approval.end() || !host_input->is_object()) {
    throw std::runtime_error("host action approval is missing its exact host input");
  }
  const std::string host_input_fingerprint = content_revision_hex(host_input->dump());

  std::optional<std::vector<std::string>> evidence_ids;
  if (!json_optional_string_list_field(approval, "evidenceIds", evidence_ids)) {
    throw std::runtime_error("approval event is missing host binding fields");
  }

  Impl::Binding binding;
  binding.approval_id = approval_id;
  binding.run_id = run_id;
  binding.trace_id = trace_id;
  binding.call_id = call_id;
  binding.tool_name = tool_name;
  binding.input_fingerprint = host_input_fingerprint;
  binding.target_host = target_host;
  binding.server_id = *server_id;
  binding.workspace_id = workspace_id;
  binding.environment = environment;
  binding.task_id = *task_id;
  binding.plan_id = *plan_id;
  binding.plan_step_id = *plan_step_id;
  binding.evidence_ids = std::move(evidence_ids);
  binding.expires_at = *expires_at_epoch;

  std::lock_guard lock(impl_->mutex);
  for (auto entry = impl_->entries.begin(); entry != impl_->entries.end();) {
    if (entry->second.binding.expires_at <= now) {
      entry = impl_->entries.erase(entry);
    } else {
      ++entry;
    }
  }
  if (const auto existing = impl_->entries.find(binding.approval_id);
      existing != impl_->entries.end()) {
    if (!existing->second.binding.same_request(binding)) {
      throw std::runtime_error("approval id is already bound to another host action");
    }
    if (existing->second.phase != Impl::Phase::Pending) {
      throw std::runtime_error("approval request is already being answered");
    }
    // A restored sidecar re-emits the same pending approval ID with a fresh
    // deadline. Keep the host ticket aligned with that event.
    existing->second.binding.expires_at = binding.expires_at;
    return true;
  }
  if (impl_->entries.size() >= kMaxTickets) {
    throw std::runtime_error("host action approval ledger is full");
  }
  const std::string key = binding.approval_id;
  impl_->entries.emplace(key, Impl::Entry{std::move(binding)});
  return true;
}

std::optional<HostApprovalLedger::ResponseToken> HostApprovalLedger::begin_response(
    const std::string& approval_id, const std::string& run_id,
    const std::string& decision, std::int64_t now) {
  std::lock_guard lock(impl_->mutex);
  const auto found = impl_->entries.find(approval_id);
  if (found == impl_->entries.end()) return std::nullopt;
  auto& entry = found->second;
  if (entry.binding.expires_at <= now) {
    impl_->entries.erase(found);
    throw std::runtime_error("host action approval has expired");
  }
  if (entry.binding.run_id != run_id) {
    throw std::runtime_error("host action approval run id does not match");
  }
  if (entry.phase != Impl::Phase::Pending) {
    throw std::runtime_error("host action approval was already answered");
  }
  bool approve_once = false;
  if (decision == "approve_once") {
    approve_once = true;
  } else if (decision == "reject") {
    approve_once = false;
  } else if (decision == "approve_session") {
    throw std::runtime_error(
        "host action approvals are per call and cannot be session grants");
  } else {
    throw std::runtime_error("approval decision is invalid");
  }
  entry.phase = Impl::Phase::Responding;
  return ResponseToken{approval_id, approve_once};
}

void HostApprovalLedger::finish_response(const ResponseToken& token, bool accepted,
                                         std::int64_t now) {
  std::lock_guard lock(impl_->mutex);
  const auto found = impl_->entries.find(token.approval_id);
  if (found == impl_->entries.end()) return;
  auto& entry = found->second;
  if (entry.phase != Impl::Phase::Responding) {
    throw std::runtime_error("host action approval response is no longer active");
  }
  const bool approved =
      accepted && token.approve_once && entry.binding.expires_at > now;
  if (approved) {
    entry.phase = Impl::Phase::Approved;
  } else {
    impl_->entries.erase(found);
  }
  impl_->changed.notify_all();
}

std::optional<HostApprovalLedger::TicketError> HostApprovalLedger::consume(
    const ExecutionBinding& request, const std::function<std::int64_t()>& now) {
  std::unique_lock lock(impl_->mutex);
  for (;;) {
    const auto found = impl_->entries.find(request.approval_id);
    if (found == impl_->entries.end()) return TicketError::Missing;
    auto& entry = found->second;
    if (entry.binding.expires_at <= now()) {
      impl_->entries.erase(found);
      return TicketError::Expired;
    }
    switch (entry.phase) {
      case Impl::Phase::Pending:
        return TicketError::NotApproved;
      case Impl::Phase::Responding:
        impl_->changed.wait_for(lock, std::chrono::seconds(1));
        continue;
      case Impl::Phase::Approved: {
        const bool matched = entry.binding.matches(request);
        impl_->entries.erase(found);
        return matched ? std::nullopt
                       : std::optional<TicketError>(TicketError::Mismatch);
      }
    }
  }
}

}  // namespace yukinal::desktop_host

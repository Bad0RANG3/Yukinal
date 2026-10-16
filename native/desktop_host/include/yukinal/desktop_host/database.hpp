#pragma once

#include <filesystem>
#include <mutex>
#include <nlohmann/json.hpp>
#include <optional>
#include <sqlite3.h>
#include <string>
#include <vector>

namespace yukinal::desktop_host {

struct DesktopNetworkProxyConfig {
  std::string mode = "direct";
  std::optional<std::string> credential_ref;
};

class DesktopDatabase {
 public:
  explicit DesktopDatabase(const std::filesystem::path& data_directory);
  ~DesktopDatabase();
  DesktopDatabase(const DesktopDatabase&) = delete;
  DesktopDatabase& operator=(const DesktopDatabase&) = delete;

  [[nodiscard]] nlohmann::json list_servers() const;
  [[nodiscard]] std::optional<nlohmann::json> get_server_endpoint(
      const std::string& server_id) const;
  [[nodiscard]] std::optional<nlohmann::json> get_server_connection(
      const std::string& server_id) const;
  void set_server_status(const std::string& server_id, const std::string& status,
                         const std::string& timestamp) const;
  void record_connection_activity(const std::string& id, const std::string& server_id,
                                  const std::string& title,
                                  const std::optional<std::string>& description,
                                  const std::string& outcome,
                                  const std::string& timestamp) const;
  [[nodiscard]] std::filesystem::path data_directory() const;
  [[nodiscard]] bool is_available() const;
  [[nodiscard]] std::optional<nlohmann::json> get_identity(
      const std::string& identity_id) const;
  void add_server(const nlohmann::json& server,
                  const std::optional<nlohmann::json>& new_identity,
                  const std::string& activity_id) const;
  [[nodiscard]] std::optional<std::string> update_server(
      const nlohmann::json& server,
      const std::optional<nlohmann::json>& new_identity,
      const std::string& activity_id) const;
  [[nodiscard]] std::optional<std::string> delete_server(
      const std::string& server_id, const std::string& activity_id,
      const std::string& timestamp) const;
  [[nodiscard]] std::vector<std::string> delete_identity_if_unshared(
      const std::string& identity_id, const std::string& except_server_id) const;
  void queue_credential_cleanup(const std::string& reference,
                                const std::string& error,
                                const std::string& timestamp) const;
  [[nodiscard]] std::vector<std::string> credential_cleanup_references() const;
  void clear_credential_cleanup(const std::string& reference) const;
  [[nodiscard]] nlohmann::json list_workspaces() const;
  [[nodiscard]] DesktopNetworkProxyConfig network_proxy_config() const;
  void save_network_proxy_config(const DesktopNetworkProxyConfig& config) const;
  [[nodiscard]] nlohmann::json list_activities(
      const std::optional<std::string>& server_id, int limit) const;
  [[nodiscard]] nlohmann::json list_tool_executions(
      const std::optional<std::string>& trace_id,
      const std::optional<std::string>& server_id, int limit) const;
  [[nodiscard]] nlohmann::json list_file_transfers(
      const std::optional<std::string>& server_id, int limit) const;
  [[nodiscard]] std::optional<nlohmann::json> get_file_transfer(
      const std::string& transfer_id) const;
  [[nodiscard]] nlohmann::json list_mcp_servers() const;
  [[nodiscard]] std::optional<nlohmann::json> get_mcp_server(
      const std::string& server_id) const;
  void upsert_mcp_server(const nlohmann::json& config) const;
  [[nodiscard]] std::optional<nlohmann::json> delete_mcp_server(
      const std::string& server_id) const;
  void insert_snapshot(const nlohmann::json& snapshot) const;
  [[nodiscard]] std::optional<nlohmann::json> latest_snapshot(
      const std::string& server_id) const;
  [[nodiscard]] nlohmann::json list_investigation_tasks(
      const std::optional<std::string>& status, int limit) const;
  [[nodiscard]] std::optional<nlohmann::json> get_investigation_task(
      const std::string& task_id) const;
  [[nodiscard]] std::optional<nlohmann::json> set_investigation_task_status(
      const std::string& task_id, const std::string& status,
      const std::optional<std::string>& completed_at, const std::string& updated_at) const;
  [[nodiscard]] std::optional<std::string> active_investigation_run_id(
      const std::string& task_id) const;
  // ── Investigation write path (durable task + Agent host RPC) ────────────────
  // Not-found is reported with `std::nullopt`; validation failures throw
  // `std::invalid_argument`; any other failure throws `std::runtime_error`.
  [[nodiscard]] std::optional<nlohmann::json> create_investigation_task(
      const nlohmann::json& task) const;
  [[nodiscard]] std::optional<nlohmann::json> update_investigation_task_progress(
      const std::string& task_id, const std::string& status, const std::string& phase,
      const std::optional<std::string>& active_run_id,
      const std::optional<nlohmann::json>& last_failure, const std::string& updated_at,
      const std::optional<std::string>& completed_at) const;
  [[nodiscard]] std::optional<nlohmann::json> get_investigation_plan(
      const std::string& plan_id) const;
  [[nodiscard]] std::optional<nlohmann::json> latest_investigation_plan(
      const std::string& task_id) const;
  void save_investigation_plan(const nlohmann::json& plan) const;
  void upsert_investigation_artifact(const nlohmann::json& artifact) const;
  [[nodiscard]] nlohmann::json list_investigation_artifacts(const std::string& task_id,
                                                            int limit) const;
  [[nodiscard]] std::optional<nlohmann::json> find_investigation_evidence_in_run(
      const nlohmann::json& evidence) const;
  void record_investigation_evidence(const nlohmann::json& evidence) const;
  [[nodiscard]] std::optional<nlohmann::json> get_investigation_evidence(
      const std::string& evidence_id) const;
  [[nodiscard]] nlohmann::json search_investigation_evidence(
      const std::string& task_id, const nlohmann::json& query) const;
  [[nodiscard]] nlohmann::json list_investigation_evidence_for_run(
      const std::string& run_id, int limit) const;
  [[nodiscard]] std::optional<nlohmann::json> get_investigation_finding(
      const std::string& finding_id) const;
  void record_investigation_finding(const nlohmann::json& finding) const;
  [[nodiscard]] std::optional<nlohmann::json> get_investigation_brief(
      const std::string& brief_id) const;
  void record_investigation_brief(const nlohmann::json& brief) const;
  [[nodiscard]] std::optional<nlohmann::json> select_investigation_brief_option(
      const std::string& task_id, const std::string& brief_id,
      const std::string& option_id) const;
  [[nodiscard]] std::optional<nlohmann::json> investigation_retention_preview(
      const std::string& task_id, const std::string& cutoff_at, int limit) const;
  [[nodiscard]] std::optional<nlohmann::json> investigation_retention_prune(
      const std::string& task_id, const std::string& cutoff_at, const nlohmann::json& items,
      const std::string& audit_id, const std::string& now) const;
  // ── Investigation schedules (durable read-only triggers) ────────────────────
  [[nodiscard]] nlohmann::json list_investigation_schedules(int limit) const;
  [[nodiscard]] std::optional<nlohmann::json> get_investigation_schedule(
      const std::string& schedule_id) const;
  [[nodiscard]] nlohmann::json list_investigation_schedule_runs(
      const std::string& schedule_id, int limit) const;
  [[nodiscard]] std::optional<nlohmann::json> get_investigation_schedule_run(
      const std::string& run_id) const;
  [[nodiscard]] std::optional<nlohmann::json> create_investigation_schedule(
      const nlohmann::json& schedule) const;
  [[nodiscard]] std::optional<nlohmann::json> update_investigation_schedule(
      const nlohmann::json& schedule) const;
  [[nodiscard]] nlohmann::json claim_due_investigation_schedule_runs(
      const std::string& now, int limit) const;
  [[nodiscard]] int investigation_evidence_count_for_run(const std::string& run_id) const;
  // ── Investigation runs (durable Agent-run binding) ──────────────────────────
  [[nodiscard]] std::optional<nlohmann::json> get_investigation_run(
      const std::string& run_id) const;
  [[nodiscard]] nlohmann::json list_investigation_runs(const std::string& task_id,
                                                       int limit) const;
  void create_investigation_run(const nlohmann::json& run) const;
  [[nodiscard]] std::optional<nlohmann::json> update_investigation_run(
      const nlohmann::json& run) const;
  [[nodiscard]] std::optional<nlohmann::json> recover_investigation_task(
      const std::string& task_id, const std::string& updated_at,
      const nlohmann::json& failure) const;
  // ── Filesystem backup ledger (host-owned remote recovery copies) ─────────────
  // The backup bytes stay on the remote server; only bounded metadata lives here.
  // Not-found is reported with `std::nullopt`; validation failures throw
  // `std::invalid_argument`; any other failure throws `std::runtime_error`.
  void insert_filesystem_backup(const nlohmann::json& record) const;
  [[nodiscard]] std::optional<nlohmann::json> find_available_filesystem_backup(
      const std::string& server_id, const std::string& path,
      const std::string& backup_path) const;
  [[nodiscard]] nlohmann::json list_filesystem_backups_for_task(
      const std::string& server_id, const std::string& task_id,
      const std::optional<std::string>& status, const std::optional<std::string>& path,
      int limit) const;
  [[nodiscard]] nlohmann::json list_available_filesystem_backups_for_server(
      const std::string& server_id, const std::optional<std::string>& path_prefix,
      int limit) const;
  void mark_filesystem_backup_restored(const std::string& server_id, const std::string& path,
                                       const std::string& backup_path,
                                       const std::string& now) const;
  void mark_filesystem_backup_deleted(const std::string& server_id, const std::string& path,
                                      const std::string& backup_path,
                                      const std::string& now) const;
  // ── Host tool call idempotency ledger ───────────────────────────────────────
  // The row records a call's intent and terminal state, never its output: a
  // guarded call may replay only while the host process still holds the response.
  // `claim_host_tool_call` returns `{"claimed":true}` for a fresh reservation or
  // `{"claimed":false,"requestFingerprint":...,"status":...}` for an existing key.
  // A row that is not `running` cannot be finished (`std::invalid_argument`).
  [[nodiscard]] std::optional<std::string> host_tool_call_status(
      const std::string& trace_id, const std::string& call_id) const;
  [[nodiscard]] nlohmann::json claim_host_tool_call(const nlohmann::json& input) const;
  void finish_host_tool_call(const std::string& trace_id, const std::string& call_id,
                             const std::string& status, const std::string& ended_at) const;
  /** SHA-256 (lowercase hex) of the canonical JSON serialization used for evidence. */
  [[nodiscard]] static std::string content_hash(const nlohmann::json& content);
  [[nodiscard]] nlohmann::json list_chat_sessions(
      const std::optional<std::string>& query, const std::optional<bool>& archived,
      int limit, int offset) const;
  [[nodiscard]] nlohmann::json get_chat_session(const std::string& session_id) const;
  [[nodiscard]] nlohmann::json create_chat_session(
      const std::string& id, const std::optional<std::string>& workspace_id,
      const std::optional<std::string>& server_id, const std::string& title,
      const std::string& timestamp) const;
  [[nodiscard]] nlohmann::json append_chat_message(
      const std::string& id, const std::string& session_id, const std::string& role,
      const std::string& content, const std::optional<nlohmann::json>& parts,
      const std::optional<std::string>& trace_id, const std::string& timestamp) const;
  [[nodiscard]] nlohmann::json set_chat_session_archived(
      const std::string& session_id,
      const std::optional<std::string>& archived_at) const;
  [[nodiscard]] nlohmann::json rename_chat_session(
      const std::string& session_id, const std::string& title) const;
  [[nodiscard]] nlohmann::json delete_chat_session(const std::string& session_id) const;

 private:
  sqlite3* connection_ = nullptr;
  std::filesystem::path data_directory_;
  mutable std::recursive_mutex mutex_;
};

[[nodiscard]] std::filesystem::path default_data_directory();

}  // namespace yukinal::desktop_host

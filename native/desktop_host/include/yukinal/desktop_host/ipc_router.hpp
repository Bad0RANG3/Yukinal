#pragma once

#include "yukinal/desktop_host/credential_store.hpp"
#include "yukinal/desktop_host/database.hpp"
#include "yukinal/desktop_host/desktop_approval.hpp"
#include "yukinal/desktop_host/desktop_mcp.hpp"
#include "yukinal/desktop_host/desktop_terminal.hpp"
#include "yukinal/desktop_host/local_paths.hpp"
#include "yukinal/desktop_host/remote_drag.hpp"
#include "yukinal/desktop_host/sidecar.hpp"
#include "yukinal/desktop_host/transfer_manager.hpp"

#include <atomic>
#include <chrono>
#include <functional>
#include <map>
#include <memory>
#include <mutex>
#include <nlohmann/json.hpp>
#include <optional>
#include <set>
#include <string>
#include <unordered_map>
#include <utility>
#include <vector>

namespace yukinal::desktop_host {

struct IpcReply {
  bool ok;
  nlohmann::json value;
};

class IpcRouter {
 public:
  explicit IpcRouter(DesktopDatabase* database = nullptr,
                     const CredentialStore* credentials = nullptr);

  ~IpcRouter();

  IpcReply invoke(const std::string& method, nlohmann::json params);
  [[nodiscard]] std::set<std::string> subscribers(const std::string& event) const;

  // The Agent→host request entry point the sidecar dispatches through. Exposed so
  // the host RPC surface can be exercised directly in tests. `cancel`, when set,
  // lets a long-running tool call observe a `host.tool.cancel` for its request.
  [[nodiscard]] nlohmann::json handle_host_request(
      const std::string& method, const nlohmann::json& params,
      const std::shared_ptr<std::atomic<bool>>& cancel = nullptr);

  // Delivers Agent sidecar notifications (for example `agent.started`) to the
  // WebView bridge. Set once at startup; safe to call from the sidecar thread.
  void set_event_sink(std::function<void(const std::string&, const nlohmann::json&)> sink);

  // Test seam: replace the lazily created transfer manager (for example with one
  // backed by an in-memory backend and history). Passing nullptr restores lazy
  // creation. The previous manager's event subscription is released first.
  void set_transfer_manager(std::unique_ptr<TransferManager> manager);

  // Test seam: the local path handle table, so a test can register files and
  // directories and then drive the transfer start commands without a native
  // picker.
  [[nodiscard]] LocalPathHandles& local_paths() { return local_paths_; }

  // Test seams for the native drag-out staging. `set_remote_drag_probe` injects the
  // read-only remote seam (lstat) and `set_remote_drag_store` injects the private
  // staging root, so `file_prepare_remote_drag` is exercisable without a server.
  // `set_remote_drag_wait_bounds` shortens the otherwise 30-minute download bound
  // and the 60-second cancellation grace so a test can drive a timeout.
  void set_remote_drag_probe(std::shared_ptr<RemoteDragProbe> probe);
  void set_remote_drag_store(std::unique_ptr<RemoteDragStore> store);
  void set_remote_drag_wait_bounds(std::chrono::milliseconds download_timeout,
                                   std::chrono::milliseconds cancel_grace);
  // Test seams for the native drag start: the OS drag seam and the main window
  // handle it drags from. Both default to null so `file_drag_out_start` fails
  // closed until the host wires them.
  void set_native_file_drag(std::shared_ptr<NativeFileDrag> drag);
  void set_native_window(void* window);

  // Registers OS-dropped local paths and publishes `file.local_dropped` to the
  // WebView. Called from the native window's drop handler on the UI thread.
  void emit_local_drop(std::vector<std::filesystem::path> paths);

  // Performs one validated non-MCP `host.tool.execute` call against a resolved
  // remote server (a read tool such as server.info/logs/services, or one that the
  // host cannot run itself and must refuse). The tool name, server id and input
  // have already passed the durable-plan, guardrail, target and call-ledger gates.
  // Returns the tool output on success and throws on failure; the default uses the
  // in-process C++ SSH core and the Agent normalizer.
  using HostToolRunner = std::function<nlohmann::json(
      const std::string& tool_name, const std::string& server_id,
      const nlohmann::json& input, const std::shared_ptr<std::atomic<bool>>& cancel)>;

  // Test seam: replace the default non-MCP tool runner (for example with a fake
  // that returns fixed output), so the request gating and dispatch are
  // exercisable without a live SSH server. Passing an empty function restores the
  // default desktop-SSH runner.
  void set_host_tool_runner(HostToolRunner runner);

 private:
  IpcReply subscribe(const nlohmann::json& params, bool add);
  IpcReply server_command(const std::string& method,
                          const nlohmann::json& params);
  IpcReply network_proxy_command(const std::string& method,
                                 nlohmann::json& params);
  IpcReply host_key_command(const std::string& method,
                            const nlohmann::json& params);
  IpcReply server_read_command(const std::string& method,
                               const nlohmann::json& params);
  IpcReply connection_command(const std::string& method,
                              const nlohmann::json& params);
  IpcReply transfer_command(const std::string& method, const nlohmann::json& params);
  IpcReply remote_drag_prepare_command(const nlohmann::json& params);
  IpcReply remote_drag_start_command(const nlohmann::json& params);
  IpcReply terminal_command(const std::string& method,
                            const nlohmann::json& params);
  [[nodiscard]] std::string issue_probe_ticket(const std::string& server_id,
                                               const std::string& host, int port,
                                               const std::string& fingerprint);
  [[nodiscard]] std::optional<std::string> claim_probe_ticket(
      const std::string& ticket, const std::string& server_id, const std::string& host,
      int port, const std::string& fingerprint);
  IpcReply agent_command(const std::string& method, const nlohmann::json& params);
  IpcReply agent_run_command(const std::string& method, const nlohmann::json& params);
  IpcReply mcp_command(const std::string& method, const nlohmann::json& params);
  IpcReply provider_command(const std::string& method, const nlohmann::json& params);
  SidecarClient& ensure_sidecar();
  void configure_sidecar();

  // `host.tool.execute`: validates the envelope, durable plan, target and call
  // ledger, then runs the tool. Remote tools go through `host_tool_runner_` (or
  // the default desktop-SSH runner); MCP tools dispatch to the host's MCP
  // supervisor after the same gates. Records the terminal outcome either way.
  [[nodiscard]] nlohmann::json host_tool_execute_command(
      const nlohmann::json& params, const std::shared_ptr<std::atomic<bool>>& cancel);
  [[nodiscard]] nlohmann::json default_host_tool_run(
      const std::string& tool_name, const std::string& server_id,
      const nlohmann::json& input, const std::shared_ptr<std::atomic<bool>>& cancel);

  // Lazily creates the SFTP-backed transfer manager. Returns nullptr when the
  // host has no local database or this build has no SSH core (so no backend can
  // be built); a recovery failure during creation propagates as `TransferError`.
  [[nodiscard]] TransferManager* ensure_transfer_manager();
  void subscribe_transfer_events_locked();

  // Lazily creates the remote drag-out read probe and staging store. The probe is
  // nullptr when the host has no database or this build has no SSH core; the store
  // is nullptr without a database. Both are guarded by `drag_mutex_`.
  [[nodiscard]] std::shared_ptr<RemoteDragProbe> ensure_remote_drag_probe();
  [[nodiscard]] RemoteDragStore* ensure_remote_drag_store();

  mutable std::mutex mutex_;
  DesktopDatabase* database_;
  DesktopCredentialStore default_credentials_;
  const CredentialStore* credentials_;
  std::unordered_map<std::string, std::set<std::string>> subscriptions_;
  std::unique_ptr<SidecarClient> sidecar_;
  std::mutex sidecar_mutex_;
  std::unique_ptr<TerminalManager> terminals_;
  std::unique_ptr<McpSupervisor> mcp_;
  LocalPathHandles local_paths_;
  // The transfer manager streams uploaded/downloaded bytes on its own worker
  // threads. It is created lazily on first use so a host with no SSH core or no
  // initialized database never opens one, and it is shut down from the
  // destructor. `transfer_mutex_` guards only the pointer and its subscription;
  // the manager's own operations are independently synchronized.
  std::mutex transfer_mutex_;
  std::unique_ptr<TransferManager> transfers_;
  std::size_t transfers_subscription_ = 0;
  // Native drag-out staging. `drag_mutex_` guards the probe/store handles and the
  // wait bounds; `prepare_mutex_` serializes preparation so at most one
  // download-to-stage runs at once (IPC dispatch already runs on the window
  // thread). The store owns each prepared directory until the native drag consumes
  // its one-time ticket.
  std::mutex drag_mutex_;
  std::shared_ptr<RemoteDragProbe> drag_probe_;
  std::unique_ptr<RemoteDragStore> drag_store_;
  std::shared_ptr<NativeFileDrag> native_drag_;
  void* native_window_ = nullptr;
  std::mutex prepare_mutex_;
  std::chrono::milliseconds remote_drag_timeout_{std::chrono::minutes(30)};
  std::chrono::milliseconds remote_drag_cancel_grace_{std::chrono::seconds(60)};
  struct ProbeTicket {
    std::string server_id;
    std::string host;
    int port = 0;
    std::string fingerprint;
    std::chrono::steady_clock::time_point expires_at;
  };
  std::map<std::string, ProbeTicket> probe_tickets_;
  // Bounded in-memory replay cache for guarded host tool calls. The durable
  // ledger records only intent/terminal state; a response may be replayed only
  // while this process still holds it (matching the retired Rust host).
  std::mutex host_tool_replay_mutex_;
  std::map<std::pair<std::string, std::string>, nlohmann::json> host_tool_replays_;
  // The non-MCP `host.tool.execute` runner. Empty means the default desktop-SSH
  // runner; a test injects a fake through `set_host_tool_runner`. Guarded by
  // `host_tool_replay_mutex_` only for the pointer swap.
  HostToolRunner host_tool_runner_;
  // In-flight OAuth authorizations, keyed by server id. `mcp_oauth_connect`
  // blocks while the browser redirect or device-code poll runs, so a token lets
  // `mcp_oauth_cancel` (on another thread) stop it.
  std::mutex oauth_flows_mutex_;
  std::map<std::string, std::shared_ptr<std::atomic<bool>>> oauth_flows_;
  // Host-issued, one-time tickets for effectful SSH actions. Filled from
  // `agent.waiting_approval` notifications and consumed by
  // `host.ssh.effectful_action_target`.
  HostApprovalLedger approvals_;
  std::function<void(const std::string&, const nlohmann::json&)> event_sink_;
};

[[nodiscard]] bool is_external_url(const std::string& url);
[[nodiscard]] bool open_external_url(const std::string& url);
[[nodiscard]] std::string operating_system();

}  // namespace yukinal::desktop_host

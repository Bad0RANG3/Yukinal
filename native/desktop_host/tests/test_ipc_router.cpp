#include "yukinal/desktop_host/ipc_router.hpp"
#include "yukinal/desktop_host/local_paths.hpp"

#include <chrono>
#include <cstdint>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <stdexcept>
#include <string>
#include <vector>

namespace {

void check(bool condition, const char* message) {
  if (condition) return;
  std::cerr << message << '\n';
  std::exit(1);
}

// A self-removing scratch directory for the local path handle tests.
struct TemporaryDirectory {
  std::filesystem::path path =
      std::filesystem::temp_directory_path() /
      ("yukinal-local-paths-test-" +
       std::to_string(std::chrono::steady_clock::now().time_since_epoch().count()));

  TemporaryDirectory() { std::filesystem::create_directories(path); }
  ~TemporaryDirectory() {
    std::error_code error;
    std::filesystem::remove_all(path, error);
  }
};

void write_binary(const std::filesystem::path& path, const std::vector<std::uint8_t>& bytes) {
  std::ofstream stream(path, std::ios::binary | std::ios::trunc);
  stream.write(reinterpret_cast<const char*>(bytes.data()),
               static_cast<std::streamsize>(bytes.size()));
}

template <typename F>
void expect_runtime_error(F&& call, const char* message) {
  try {
    call();
  } catch (const std::runtime_error&) {
    return;
  }
  std::cerr << message << '\n';
  std::exit(1);
}

}  // namespace

int main() {
  using nlohmann::json;
  using yukinal::desktop_host::IpcRouter;

  IpcRouter router;
  const auto ping = router.invoke("core_ping", json::object());
  check(ping.ok, "core_ping should succeed");
  check(ping.value.value("version", "") == YUKINAL_DESKTOP_VERSION,
        "core_ping should report the app version");
  check(ping.value.contains("os"), "core_ping should report the platform");

  check(!router.invoke("core_ping", {{"unexpected", true}}).ok,
        "core_ping should reject unexpected parameters");
  check(!router.invoke("server_list", json::object()).ok,
        "server_list should report an unavailable database");
  check(!router.invoke("activity_list", {{"limit", 101}}).ok,
        "activity_list should reject limits greater than 100");
  check(!router.invoke("activity_list", {{"serverId", "not-a-server-id"}}).ok,
        "activity_list should reject malformed server ids");
  check(!router.invoke("activity_list", json::object()).ok,
        "activity_list should report a missing local database");
  check(!router.invoke("tool_execution_list", {{"traceId", ""}}).ok,
        "tool_execution_list should reject an empty trace id");
  check(!router.invoke("tool_execution_list", {{"serverId", "invalid"}}).ok,
        "tool_execution_list should reject malformed server ids");
  check(!router.invoke("tool_execution_list", {{"limit", 0}}).ok,
        "tool_execution_list should reject a zero limit");
  check(!router.invoke("tool_execution_list", json::object()).ok,
        "tool_execution_list should report a missing local database");
  check(!router.invoke("shell.open_url", {{"url", "javascript:alert(1)"}}).ok,
        "external URL opening should reject executable schemes");
  check(yukinal::desktop_host::is_external_url("https://example.com/path"),
        "HTTPS URLs should be accepted");
  check(yukinal::desktop_host::is_external_url("mailto:ops@example.com"),
        "mailto URLs should be accepted");
  check(!yukinal::desktop_host::is_external_url("https://example.com\n"),
        "external URLs should reject control characters");

  const auto subscription = router.invoke("__events.subscribe",
      {{"event", "agent.completed"}, {"token", "01234567-89ab-cdef-0123-456789abcdef"}});
  check(subscription.ok, "valid event subscriptions should succeed");
  check(router.subscribers("agent.completed").size() == 1,
        "subscribers should be tracked by event name");
  check(!router.invoke("__events.subscribe",
      {{"event", "agent.completed"}, {"token", "short"}}).ok,
      "invalid subscription tokens should be rejected");
  const auto unsubscribe = router.invoke("__events.unsubscribe",
      {{"event", "agent.completed"}, {"token", "01234567-89ab-cdef-0123-456789abcdef"}});
  check(unsubscribe.ok && router.subscribers("agent.completed").empty(),
        "event unsubscription should release its callback token");

  check(!router.invoke("local_file_preview", nlohmann::json::object()).ok,
        "local_file_preview should require a handle id");
  check(!router.invoke("local_file_preview", {{"handleId", "local_path_missing"}}).ok,
        "local_file_preview should reject an unknown handle");
  check(!router.invoke("local_path_pick", {{"kind", "everything"}}).ok,
        "local_path_pick should reject an unknown kind");

  check(!router.invoke("file_transfer_list", {{"unexpected", true}}).ok,
        "file_transfer_list should reject unknown parameters");
  check(!router.invoke("file_transfer_list", {{"serverId", "not-a-server"}}).ok,
        "file_transfer_list should reject malformed server ids");
  check(!router.invoke("file_transfer_list", nlohmann::json::object()).ok,
        "file_transfer_list should report a missing local database");
  check(!router.invoke("file_transfer_get", nlohmann::json::object()).ok,
        "file_transfer_get should require a transfer id");
  check(!router.invoke("file_transfer_get", {{"transferId", ""}}).ok,
        "file_transfer_get should reject an empty transfer id");
  check(!router.invoke("file_transfer_get", {{"transferId", "trf_01"}}).ok,
        "file_transfer_get should report a missing local database");

  check(!router.invoke("server_logs", nlohmann::json::object()).ok,
        "server_logs should require a server id");
  check(!router.invoke("server_logs", {{"serverId", "not-a-server"}}).ok,
        "server_logs should reject a malformed server id");
  check(!router.invoke("server_logs", {{"serverId", "srv_ok"}}).ok,
        "server_logs should report a missing local database");
  check(!router.invoke("server_services", nlohmann::json::object()).ok,
        "server_services should require a server id");
  check(!router.invoke("server_services", {{"serverId", "srv_ok"}}).ok,
        "server_services should report a missing local database");
  // Python deliberately omits timestamps, including on degraded collectors.
  // The shared builder used by both UI and Agent tool routes completes the
  // strict snapshot contract without dropping a failing collector's evidence.
  json normalized_snapshot = {{"health", "warning"},
                              {"capabilities", {{"linux", true}, {"docker", false}}},
                              {"collectors", json::array()}};
  for (const auto* id : {"os", "cpu", "memory", "uptime", "disk", "network", "docker"}) {
    normalized_snapshot["collectors"].push_back({{"collectorId", id}, {"ok", true}});
  }
  normalized_snapshot["collectors"][1]["ok"] = false;
  normalized_snapshot["collectors"][1]["error"] = "CPU sample unavailable";
  normalized_snapshot["collectors"][6]["collectedAt"] = "stale-agent-time";
  const auto snapshot = yukinal::desktop_host::detail::make_snapshot_record(
      "srv_local_snapshot", normalized_snapshot);
  check(snapshot.at("id").get<std::string>().starts_with("snap_srv_local_snapshot_") &&
            snapshot.at("serverId") == "srv_local_snapshot",
        "snapshot identity should be supplied by the desktop host");
  check(snapshot.at("collectedAt").is_string() &&
            !snapshot.at("collectedAt").get<std::string>().empty() &&
            snapshot.at("collectors").size() == 7,
        "the normalized seven-collector batch should receive a collection timestamp");
  for (const auto& collector : snapshot.at("collectors")) {
    check(collector.at("collectedAt") == snapshot.at("collectedAt"),
          "all collector timestamps must match the host-owned batch timestamp");
  }
  check(snapshot.at("collectors")[1].at("ok") == false &&
            snapshot.at("collectors")[1].at("error") == "CPU sample unavailable" &&
            snapshot.at("capabilities") == normalized_snapshot.at("capabilities") &&
            !normalized_snapshot.at("collectors")[0].contains("collectedAt") &&
            normalized_snapshot.at("collectors")[6].at("collectedAt") == "stale-agent-time",
        "timestamp completion must preserve collector failures and not mutate normalized input");
  auto invalid_snapshot = normalized_snapshot;
  invalid_snapshot["collectors"] = json::object();
  expect_runtime_error(
      [&] { (void)yukinal::desktop_host::detail::make_snapshot_record("srv_local_snapshot", invalid_snapshot); },
      "snapshot construction should reject a non-array collector collection");
  invalid_snapshot["collectors"] = json::array({nullptr});
  expect_runtime_error(
      [&] { (void)yukinal::desktop_host::detail::make_snapshot_record("srv_local_snapshot", invalid_snapshot); },
      "snapshot construction should reject malformed collector records");
  invalid_snapshot["collectors"] = json::array();
  check(yukinal::desktop_host::detail::make_snapshot_record("srv_local_snapshot", invalid_snapshot)
            .at("collectors").empty(),
        "an empty collector batch should remain a valid partial snapshot");

  check(!router.invoke("server_snapshot", nlohmann::json::object()).ok,
        "server_snapshot should require a server id");
  check(!router.invoke("server_snapshot", {{"serverId", "not-a-server"}}).ok,
        "server_snapshot should reject a malformed server id");
  check(!router.invoke("server_snapshot", {{"serverId", "srv_ok"}}).ok,
        "server_snapshot should report a missing local database");
  check(!router.invoke("remote_file_list", {{"serverId", "srv_ok"}}).ok,
        "remote_file_list should require a path");
  check(!router.invoke("remote_file_read", nlohmann::json::object()).ok,
        "remote_file_read should require serverId and path");
  check(!router.invoke("remote_file_preview",
                       {{"serverId", "srv_ok"}, {"path", "/etc/hosts"}})
               .ok,
        "remote_file_preview should report a missing local database");
  check(!router.invoke("server_connect", {{"serverId", "srv_ok"}}).ok,
        "server_connect should report a missing local database");
  check(!router.invoke("server_disconnect", {{"serverId", "srv_ok"}}).ok,
        "server_disconnect should report a missing local database");
  check(!router.invoke("server_auth_respond",
                       {{"authId", "auth_x"}, {"responses", nlohmann::json::array({"otp"})}})
               .ok,
        "server_auth_respond should report a missing local database");
  check(!router.invoke("terminal_open",
                       {{"serverId", "srv_ok"}, {"cols", 80}, {"rows", 24}})
               .ok,
        "terminal_open should report a missing local database");
  check(!router.invoke("terminal_write",
                       {{"terminalSessionId", "term_x"}, {"data", "ls\n"}})
               .ok,
        "terminal_write should report a missing local database");
  check(!router.invoke("mcp_server_start", {{"serverId", "mcp_echo"}}).ok,
        "mcp_server_start should report a missing local database");
  check(!router.invoke("mcp_server_stop", {{"serverId", "mcp_echo"}}).ok,
        "mcp_server_stop should report a missing local database");
  check(!router.invoke("server_host_key_probe", nlohmann::json::object()).ok,
        "server_host_key_probe should require a server id");
  check(!router.invoke("server_host_key_probe", {{"serverId", "srv_ok"}}).ok,
        "server_host_key_probe should report a missing local database");
  check(!router.invoke("server_host_key_trust",
                       {{"serverId", "srv_ok"}, {"probeTicket", "probe_x"}})
               .ok,
        "server_host_key_trust should require a fingerprint");
  check(!router.invoke("server_host_key_trust",
                       {{"serverId", "srv_ok"},
                        {"probeTicket", "probe_x"},
                        {"fingerprint", "SHA256:short"}})
               .ok,
        "server_host_key_trust should reject an invalid fingerprint");

  using yukinal::desktop_host::kLocalHandleTtlMillis;
  using yukinal::desktop_host::kMaxFilePreviewBytes;
  using yukinal::desktop_host::LocalPathHandles;
  using yukinal::desktop_host::make_file_preview;

  TemporaryDirectory directory;
  const auto text_path = directory.path / "note.txt";
  write_binary(text_path, {'h', 'e', 'l', 'l', 'o', '\n'});

  LocalPathHandles handles;
  auto registered = handles.register_paths({text_path});
  check(registered.handles.size() == 1 && registered.rejected == 0,
        "a regular file should register exactly one handle");
  const auto& file_handle = registered.handles.front();
  check(file_handle.kind == "file" && file_handle.name == "note.txt",
        "a file handle should report its kind and name");
  check(file_handle.size.has_value() && *file_handle.size == 6,
        "a file handle should report its size");

  const auto preview = handles.read_preview(file_handle.handle_id, kMaxFilePreviewBytes);
  check(preview.name == "note.txt" && preview.size == 6 &&
            std::string(preview.bytes.begin(), preview.bytes.end()) == "hello\n",
        "read_preview should return the bounded file bytes");
  const auto rendered = make_file_preview(preview.name, preview.size, preview.bytes,
                                          kMaxFilePreviewBytes);
  check(rendered.at("kind") == "text" && rendered.at("text") == "hello\n" &&
            rendered.at("truncated") == false,
        "a valid UTF-8 file should preview as text");

  const auto truncated =
      make_file_preview(preview.name, preview.size, preview.bytes, 4);
  check(truncated.at("truncated") == true && truncated.at("text") == "hell",
        "a body longer than the cap should preview as truncated text");

  const auto incomplete =
      make_file_preview("partial.txt", 4, {'a', 'b', 'c', 0xC3}, kMaxFilePreviewBytes);
  check(incomplete.at("kind") == "text" && incomplete.at("text") == "abc" &&
            incomplete.at("truncated") == true,
        "an incomplete trailing UTF-8 sequence should still preview its readable prefix");

  const auto binary = make_file_preview("blob.bin", 3, {'a', 0x00, 'b'}, kMaxFilePreviewBytes);
  check(binary.at("kind") == "binary" && binary.at("text").is_null(),
        "bytes with a NUL should preview as binary");

  const auto png = make_file_preview("icon.png", 8,
                                     {0x89, 'P', 'N', 'G', '\r', '\n', 0x1A, '\n'},
                                     kMaxFilePreviewBytes);
  check(png.at("kind") == "image" &&
            png.at("dataUrl").get<std::string>().rfind("data:image/png;base64,", 0) == 0,
        "a PNG header should preview as a base64 image");

  const auto directory_handle = handles.register_paths({directory.path});
  check(directory_handle.handles.size() == 1 &&
            directory_handle.handles.front().kind == "directory",
        "a directory should register as a directory handle");
  bool directory_rejected = false;
  try {
    (void)handles.read_preview(directory_handle.handles.front().handle_id,
                               kMaxFilePreviewBytes);
  } catch (const std::exception&) {
    directory_rejected = true;
  }
  check(directory_rejected, "read_preview should refuse a directory handle");

  std::int64_t fake_now = 1'000;
  LocalPathHandles expiring([&fake_now] { return fake_now; });
  const auto expiring_handle = expiring.register_paths({text_path});
  check(expiring_handle.handles.size() == 1, "the expiring handle should register");
  fake_now += kLocalHandleTtlMillis + 1;
  bool expired = false;
  try {
    (void)expiring.read_preview(expiring_handle.handles.front().handle_id,
                                kMaxFilePreviewBytes);
  } catch (const std::exception&) {
    expired = true;
  }
  check(expired, "a handle should expire after its TTL");

  bool missing_rejected = false;
  try {
    (void)handles.read_preview("local_path_unknown", kMaxFilePreviewBytes);
  } catch (const std::exception&) {
    missing_rejected = true;
  }
  check(missing_rejected, "read_preview should reject an unknown handle");

  // Upload handles are consumed exactly once and every path is revalidated with
  // no-follow semantics before a transfer may start.
  LocalPathHandles uploads;
  const auto upload_file = uploads.register_paths({text_path});
  const auto upload_dir = uploads.register_paths({directory.path});
  check(upload_file.handles.size() == 1 && upload_dir.handles.size() == 1,
        "upload sources should register one handle each");
  const auto taken = uploads.take_upload_sources(
      {upload_file.handles.front().handle_id, upload_dir.handles.front().handle_id});
  check(taken.size() == 2 && taken[0] == text_path && taken[1] == directory.path,
        "take_upload_sources should return the registered paths in order");
  expect_runtime_error(
      [&] { (void)uploads.take_upload_sources({upload_file.handles.front().handle_id}); },
      "a consumed upload handle must not be reusable");

  const auto repeated = uploads.register_paths({text_path});
  expect_runtime_error(
      [&] {
        (void)uploads.take_upload_sources({repeated.handles.front().handle_id,
                                           repeated.handles.front().handle_id});
      },
      "a repeated upload handle must be rejected");
  const auto once = uploads.take_upload_sources({repeated.handles.front().handle_id});
  check(once.size() == 1 && once[0] == text_path,
        "a rejected upload request must not consume handles");
  expect_runtime_error([&] { (void)uploads.take_upload_sources({}); },
                       "an empty upload selection must be rejected");
  expect_runtime_error(
      [&] { (void)uploads.take_upload_sources(std::vector<std::string>(257, "local_path_x")); },
      "more than 256 upload handles must be rejected");

  const auto retyped_path = directory.path / "retyped";
  write_binary(retyped_path, {'x'});
  LocalPathHandles mutating;
  const auto retyped = mutating.register_paths({retyped_path});
  std::filesystem::remove(retyped_path);
  std::filesystem::create_directory(retyped_path);
  expect_runtime_error(
      [&] { (void)mutating.take_upload_sources({retyped.handles.front().handle_id}); },
      "an upload handle that changed type must be rejected");

  bool upload_expired = false;
  try {
    (void)expiring.take_upload_sources({expiring_handle.handles.front().handle_id});
  } catch (const std::exception&) {
    upload_expired = true;
  }
  check(upload_expired, "an expired upload handle must be rejected");

  // Download destinations stay reusable but must still be a real directory.
  LocalPathHandles destinations;
  const auto destination = destinations.register_paths({directory.path});
  const auto file_destination = destinations.register_paths({text_path});
  check(destinations.take_download_directory(destination.handles.front().handle_id) ==
            directory.path,
        "take_download_directory should resolve a registered directory");
  check(destinations.take_download_directory(destination.handles.front().handle_id) ==
            directory.path,
        "a download destination should stay reusable");
  expect_runtime_error(
      [&] { (void)destinations.take_download_directory(file_destination.handles.front().handle_id); },
      "a file handle must not be a download destination");
  expect_runtime_error([&] { (void)destinations.take_download_directory("local_path_missing"); },
                       "an unknown download destination must be rejected");

  return 0;
}

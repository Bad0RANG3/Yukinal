#pragma once

#include <memory>
#include <nlohmann/json.hpp>
#include <optional>
#include <string>
#include <vector>

#include "yukinal/desktop_host/transfer_backend.hpp"

namespace yukinal::desktop_host {

class DesktopDatabase;
class CredentialStore;

// ── Streaming worker channel ───────────────────────────────────────────────────
// One byte channel to a spawned `yukinal-ssh-service` streaming worker. The
// process implementation uses anonymous pipes; a test can substitute an
// in-memory channel to drive the frame protocol without a server. Lines are
// newline-delimited JSON frames in both directions.

class SftpStreamChannel {
 public:
  virtual ~SftpStreamChannel() = default;
  virtual void write_line(const std::string& line) = 0;
  virtual void close_write() = 0;
  virtual std::optional<std::string> read_line() = 0;
  virtual int wait() = 0;
};

// Consumes the `ready` handshake, then wraps the channel as a streaming reader
// (`data` frames until `closed`) or writer (`data` frames then `close`). Both
// throw `TransferError` on a worker `error` frame, on invalid framing and on an
// unexpected close, so the manager records the correct failure kind.
std::unique_ptr<TransferReader> make_sftp_stream_reader(
    std::shared_ptr<SftpStreamChannel> channel);
std::unique_ptr<TransferWriter> make_sftp_stream_writer(
    std::shared_ptr<SftpStreamChannel> channel);

// ── Remote operation seam ──────────────────────────────────────────────────────
// The blocking SFTP surface the adapter drives, one fresh connection per call.
// It is separated from `SftpTransferBackend` so the adapter's policy (no-follow,
// fail-closed staging cleanup, unsupported atomic replace) is unit-testable
// without a server. Every method reports failure by throwing `TransferError`.

class SftpRemoteOps {
 public:
  virtual ~SftpRemoteOps() = default;

  // Throws `TransferError(Unsupported)` when the target's auth/trust cannot be
  // served by the C++ SSH core.
  virtual void ensure_supported(const std::string& server_id) = 0;
  virtual std::optional<RemoteStat> lstat(const std::string& server_id,
                                          const std::string& path) = 0;
  virtual std::vector<RemoteTransferEntry> list_dir(const std::string& server_id,
                                                    const std::string& path) = 0;
  virtual std::unique_ptr<TransferReader> open_read(const std::string& server_id,
                                                    const std::string& path) = 0;
  virtual std::unique_ptr<TransferWriter> create_exclusive(const std::string& server_id,
                                                           const std::string& path) = 0;
  virtual void create_dir(const std::string& server_id, const std::string& path) = 0;
  virtual void remove_file(const std::string& server_id, const std::string& path) = 0;
  virtual void publish_no_replace(const std::string& server_id, const std::string& source,
                                  const std::string& destination) = 0;
};

// ── Adapter ────────────────────────────────────────────────────────────────────
// The `TransferBackend` the manager drives over SFTP. It never advertises an
// atomic replace (SFTP v3 has none) and reconciles recovered staging files only
// when they are proven regular files.

class SftpTransferBackend : public TransferBackend {
 public:
  explicit SftpTransferBackend(std::shared_ptr<SftpRemoteOps> ops);

  [[nodiscard]] bool supports_atomic_replace() const override { return false; }

  std::optional<RemoteStat> lstat(const std::string& server_id,
                                  const std::string& path) override;
  std::vector<RemoteTransferEntry> list_dir(const std::string& server_id,
                                            const std::string& path) override;
  std::unique_ptr<TransferReader> open_read(const std::string& server_id,
                                            const std::string& path) override;
  std::unique_ptr<TransferWriter> create_exclusive(const std::string& server_id,
                                                   const std::string& path) override;
  void create_dir(const std::string& server_id, const std::string& path) override;
  void remove_file(const std::string& server_id, const std::string& path) override;
  void publish_no_replace(const std::string& server_id, const std::string& source,
                          const std::string& destination) override;
  void rename_replace(const std::string& server_id, const std::string& source,
                      const std::string& destination) override;
  void remove_recovered_staging(const std::string& server_id,
                                const std::string& path) override;

 private:
  std::shared_ptr<SftpRemoteOps> ops_;
};

// The production seam: resolves each target from the host database/credential
// store, runs the non-streaming operations in-process, and spawns the packaged
// SSH-service worker for streaming. Returns nullptr when this build has no SSH
// core.
std::shared_ptr<SftpRemoteOps> make_ssh_service_sftp_remote_ops(
    DesktopDatabase* database, const CredentialStore* credentials);

}  // namespace yukinal::desktop_host

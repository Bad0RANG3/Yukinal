// Unit tests for the pure transfer contract module (`transfer_types`). These
// cover the shared `TransferSnapshot` JSON shape, the host-private staging
// encoding and the path/name helpers the transfer manager relies on. They never
// touch the database or the network.

#include "yukinal/desktop_host/sftp_transfer_backend.hpp"
#include "yukinal/desktop_host/transfer_local.hpp"
#include "yukinal/desktop_host/transfer_types.hpp"

#include <algorithm>
#include <chrono>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <iterator>
#include <memory>
#include <optional>
#include <set>
#include <string>
#include <utility>
#include <vector>

namespace {

using namespace yukinal::desktop_host;

void check(bool condition, const char* message) {
  if (condition) return;
  std::cerr << message << '\n';
  std::exit(1);
}

template <typename F>
void expect_invalid(F&& call, const char* message) {
  try {
    call();
  } catch (const std::invalid_argument&) {
    return;
  }
  std::cerr << message << '\n';
  std::exit(1);
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

template <typename F>
void expect_transfer_error(F&& call, TransferErrorCode code, const char* message) {
  try {
    call();
  } catch (const TransferError& error) {
    if (error.code() == code) return;
  }
  std::cerr << message << '\n';
  std::exit(1);
}

std::vector<std::string> split(const std::string& value, char separator) {
  std::vector<std::string> parts;
  std::size_t from = 0;
  for (;;) {
    const auto position = value.find(separator, from);
    parts.push_back(value.substr(from, position == std::string::npos ? std::string::npos
                                                                    : position - from));
    if (position == std::string::npos) break;
    from = position + 1;
  }
  return parts;
}

bool all_digits(const std::string& value) {
  return !value.empty() &&
         std::all_of(value.begin(), value.end(), [](unsigned char byte) {
           return byte >= '0' && byte <= '9';
         });
}

struct TemporaryDirectory {
  std::filesystem::path path =
      std::filesystem::temp_directory_path() /
      ("yukinal-transfer-" +
       std::to_string(std::chrono::steady_clock::now().time_since_epoch().count()));
  TemporaryDirectory() { std::filesystem::create_directories(path); }
  ~TemporaryDirectory() {
    std::error_code error;
    std::filesystem::remove_all(path, error);
  }
};

void write_file(const std::filesystem::path& path, const std::string& content) {
  std::ofstream out(path, std::ios::binary);
  out << content;
}

std::string read_file(const std::filesystem::path& path) {
  std::ifstream in(path, std::ios::binary);
  return std::string(std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>());
}

// Local filesystem seam: fingerprints, exclusive staging, publication and
// restart cleanup. Exercises real files in a temporary directory.
void run_local_checks() {
  TemporaryDirectory temporary;
  const auto source = temporary.path / "source.txt";
  write_file(source, "hello world");

  const auto fingerprint = local_source_fingerprint(source);
  check(fingerprint.size == 11, "a local source fingerprint must report its size");
  verify_local_source_unchanged(source, fingerprint);
  write_file(source, "hello world!");
  bool changed = false;
  try {
    verify_local_source_unchanged(source, fingerprint);
  } catch (const TransferError& error) {
    changed = error.code() == TransferErrorCode::SourceChanged;
  }
  check(changed, "a changed local source must be reported as SourceChanged");

  const auto directory = temporary.path / "tree";
  std::filesystem::create_directories(directory / "b");
  write_file(directory / "a.txt", "a");
  write_file(directory / "b" / "c.txt", "c");
  const auto children = read_local_directory(directory);
  check(children.size() == 2 && children[0].filename() == "a.txt" &&
            children[1].filename() == "b",
        "read_local_directory must return sorted children");
  expect_transfer_error([&] { (void)read_local_directory(source); },
                        TransferErrorCode::InvalidInput,
                        "read_local_directory must reject a regular file");

  const auto target = directory / "report.txt";
  auto staging = create_local_staging_file(target, "tr_abc-def", 0);
  check(is_owned_staging_name(staging.path().filename().string(), "tr_abc-def") &&
            staging.path().filename().string().find(".yukinal-tr_abcdef-0-0.part") !=
                std::string::npos,
        "a staging file must carry the transfer-scoped basename");
  staging.write("payload", 7);
  staging.close();
  auto second = create_local_staging_file(target, "tr_abc-def", 0);
  check(second.path() != staging.path() &&
            second.path().filename().string().find("-0-1.part") != std::string::npos,
        "a second staging file must use the next attempt suffix");
  second.close();
  remove_local_staging(second.path());
  check(!std::filesystem::exists(second.path()),
        "remove_local_staging must delete the staging file");

  publish_local_no_replace(staging.path(), target);
  check(read_file(target) == "payload" && !std::filesystem::exists(staging.path()),
        "no-replace publication must move the staged content into place");
  bool duplicate = false;
  auto again = create_local_staging_file(target, "tr_abc-def", 0);
  again.write("other", 5);
  again.close();
  try {
    publish_local_no_replace(again.path(), target);
  } catch (const TransferError& error) {
    duplicate = error.code() == TransferErrorCode::InvalidInput;
  }
  check(duplicate, "no-replace publication must reject an existing destination");
  remove_local_staging(again.path());

  const auto expected = *path_fingerprint_if_regular(target);
  auto replacement = create_local_staging_file(target, "tr_abc-def", 1);
  replacement.write("replaced", 8);
  replacement.close();
  publish_local_replace(replacement.path(), target, expected);
  check(read_file(target) == "replaced",
        "replace publication must overwrite the destination");

  const auto stale = *path_fingerprint_if_regular(target);
  auto conflicting = create_local_staging_file(target, "tr_abc-def", 2);
  conflicting.write("x", 1);
  conflicting.close();
  write_file(target, "changed after staging");
  bool stale_rejected = false;
  try {
    publish_local_replace(conflicting.path(), target, stale);
  } catch (const TransferError& error) {
    stale_rejected = error.code() == TransferErrorCode::InvalidInput;
  }
  check(stale_rejected, "replace publication must reject a destination that changed");
  remove_local_staging(conflicting.path());

  check(!path_fingerprint_if_regular(temporary.path / "absent.txt").has_value(),
        "an absent destination must have no fingerprint");
  expect_transfer_error([&] { (void)path_fingerprint_if_regular(directory); },
                        TransferErrorCode::InvalidInput,
                        "a non-regular destination must be rejected");
  ensure_download_directory(directory);
  expect_transfer_error([&] { ensure_download_directory(temporary.path / "absent-dir"); },
                        TransferErrorCode::InvalidInput,
                        "a missing download directory must be rejected");
  expect_transfer_error([&] { ensure_download_directory(source); },
                        TransferErrorCode::InvalidInput,
                        "a file download destination must be rejected");

  const auto recovered = directory / ".report.txt.yukinal-tr_abcdef-3-0.part";
  write_file(recovered, "residue");
  remove_recovered_local_staging(recovered, "tr_abc-def");
  check(!std::filesystem::exists(recovered),
        "recovery must remove its own staging residue");
  remove_recovered_local_staging(recovered, "tr_abc-def");
  expect_transfer_error(
      [&] { remove_recovered_local_staging(directory / "other.txt", "tr_abc-def"); },
      TransferErrorCode::InvalidInput, "recovery must reject a non-owned path");
}

// ── Fake SFTP seams ──────────────────────────────────────────────────────────

struct FakeRemoteReader : public TransferReader {
  explicit FakeRemoteReader(std::string content) : content_(std::move(content)) {}
  std::size_t read(char* data, std::size_t size) override {
    const auto count = std::min(size, content_.size());
    if (count > 0) std::memcpy(data, content_.data(), count);
    content_.erase(0, count);
    return count;
  }
  std::string content_;
};

struct FakeRemoteWriter : public TransferWriter {
  void write(const char*, std::size_t) override {}
  void finish() override { finished = true; }
  bool finished = false;
};

// Records every call so the adapter's delegation and fail-closed policy can be
// exercised without a server.
class FakeRemoteOps : public SftpRemoteOps {
 public:
  void ensure_supported(const std::string& server_id) override {
    ensure_calls.push_back(server_id);
    if (fail_ensure) {
      throw TransferError(TransferErrorCode::Unsupported, "target unsupported");
    }
  }

  std::optional<RemoteStat> lstat(const std::string& server_id,
                                  const std::string& path) override {
    lstat_calls.emplace_back(server_id, path);
    return next_lstat;
  }

  std::vector<RemoteTransferEntry> list_dir(const std::string& server_id,
                                            const std::string& path) override {
    list_calls.emplace_back(server_id, path);
    return {};
  }

  std::unique_ptr<TransferReader> open_read(const std::string&, const std::string&) override {
    ++open_read_calls;
    return std::make_unique<FakeRemoteReader>("data");
  }

  std::unique_ptr<TransferWriter> create_exclusive(const std::string&,
                                                   const std::string&) override {
    ++create_calls;
    return std::make_unique<FakeRemoteWriter>();
  }

  void create_dir(const std::string&, const std::string&) override { ++create_dir_calls; }

  void remove_file(const std::string& server_id, const std::string& path) override {
    remove_file_calls.emplace_back(server_id, path);
  }

  void publish_no_replace(const std::string&, const std::string&, const std::string&) override {
    ++publish_calls;
  }

  std::vector<std::string> ensure_calls;
  std::vector<std::pair<std::string, std::string>> lstat_calls;
  std::vector<std::pair<std::string, std::string>> list_calls;
  std::vector<std::pair<std::string, std::string>> remove_file_calls;
  std::optional<RemoteStat> next_lstat;
  bool fail_ensure = false;
  int open_read_calls = 0;
  int create_calls = 0;
  int create_dir_calls = 0;
  int publish_calls = 0;
};

// A fully in-memory stream channel: the test queues the worker's output frames and
// inspects the frames the host writes, so the frame protocol is covered without a
// server or a spawned process.
class FakeStreamChannel : public SftpStreamChannel {
 public:
  void enqueue(std::string line) { output_.push_back(std::move(line)); }
  void write_line(const std::string& line) override { written_.push_back(line); }
  void close_write() override { write_closed = true; }
  std::optional<std::string> read_line() override {
    if (output_.empty()) return std::nullopt;
    auto line = output_.front();
    output_.erase(output_.begin());
    return line;
  }
  int wait() override { return exit_code; }

  std::vector<std::string> written_;
  bool write_closed = false;
  int exit_code = 0;

 private:
  std::vector<std::string> output_;
};

void run_sftp_backend_checks() {
  const std::string server = "srv_01abc";
  auto ops = std::make_shared<FakeRemoteOps>();
  SftpTransferBackend backend(ops);

  check(!backend.supports_atomic_replace(),
        "the SFTP adapter must not advertise an atomic replace");

  ops->next_lstat = RemoteStat{RemoteEntryKind::File, 12, std::uint32_t{5}};
  const auto stat = backend.lstat(server, "/srv/app.log");
  check(stat.has_value() && stat->size == 12 && stat->modified == 5 &&
            !ops->ensure_calls.empty() && ops->ensure_calls.back() == server &&
            ops->lstat_calls.size() == 1 && ops->lstat_calls[0].second == "/srv/app.log",
        "the SFTP adapter must check support and forward lstat");

  (void)backend.list_dir(server, "/srv");
  check(ops->list_calls.size() == 1 && ops->list_calls[0].second == "/srv",
        "the SFTP adapter must forward list_dir");
  (void)backend.open_read(server, "/srv/a");
  (void)backend.create_exclusive(server, "/srv/b");
  check(ops->open_read_calls == 1 && ops->create_calls == 1,
        "the SFTP adapter must forward streaming handles");
  backend.create_dir(server, "/srv/new");
  check(ops->create_dir_calls == 1, "the SFTP adapter must forward create_dir");
  backend.remove_file(server, "/srv/a");
  check(ops->remove_file_calls.size() == 1, "the SFTP adapter must forward remove_file");
  backend.publish_no_replace(server, "/srv/a", "/srv/b");
  check(ops->publish_calls == 1, "the SFTP adapter must forward publish_no_replace");

  expect_transfer_error([&] { backend.rename_replace(server, "/srv/a", "/srv/b"); },
                        TransferErrorCode::Unsupported,
                        "the SFTP adapter must refuse rename_replace");

  ops->next_lstat = std::nullopt;
  const auto removals = ops->remove_file_calls.size();
  backend.remove_recovered_staging(server, "/srv/.stage.part");
  check(ops->remove_file_calls.size() == removals,
        "an absent recovered staging file must not be removed");

  ops->next_lstat = RemoteStat{RemoteEntryKind::File, 1, std::nullopt};
  backend.remove_recovered_staging(server, "/srv/.stage.part");
  check(ops->remove_file_calls.size() == removals + 1 &&
            ops->remove_file_calls.back().second == "/srv/.stage.part",
        "a regular recovered staging file must be removed");

  ops->next_lstat = RemoteStat{RemoteEntryKind::Directory, 0, std::nullopt};
  expect_transfer_error([&] { backend.remove_recovered_staging(server, "/srv/.stage.part"); },
                        TransferErrorCode::InvalidInput,
                        "a non-regular recovered staging entry must be rejected");

  ops->fail_ensure = true;
  const auto lists = ops->list_calls.size();
  expect_transfer_error([&] { (void)backend.list_dir(server, "/srv"); },
                        TransferErrorCode::Unsupported,
                        "an unsupported target must fail closed");
  check(ops->list_calls.size() == lists,
        "an unsupported target must not reach the remote operation");
}

void run_sftp_stream_reader_checks() {
  auto channel = std::make_shared<FakeStreamChannel>();
  channel->enqueue("{\"type\":\"ready\"}");
  channel->enqueue("{\"type\":\"data\",\"data\":\"aGVsbG8g\"}");
  channel->enqueue("{\"type\":\"data\",\"data\":\"d29ybGQ=\"}");
  channel->enqueue("{\"type\":\"closed\",\"exit_code\":0,\"bytes\":11}");
  auto reader = make_sftp_stream_reader(channel);
  std::string collected;
  char buffer[4];
  for (;;) {
    const auto count = reader->read(buffer, sizeof(buffer));
    if (count == 0) break;
    collected.append(buffer, count);
  }
  check(collected == "hello world",
        "the SFTP reader must decode data frames across caller-sized reads");

  auto error_channel = std::make_shared<FakeStreamChannel>();
  error_channel->enqueue("{\"type\":\"ready\"}");
  error_channel->enqueue("{\"type\":\"error\",\"code\":\"sftp_error\",\"message\":\"boom\"}");
  auto error_reader = make_sftp_stream_reader(error_channel);
  expect_transfer_error([&] { char block[8]; (void)error_reader->read(block, sizeof(block)); },
                        TransferErrorCode::Remote,
                        "an SFTP download error frame must fail the read");

  auto bad_channel = std::make_shared<FakeStreamChannel>();
  bad_channel->enqueue("{\"type\":\"ready\"}");
  bad_channel->enqueue("{\"type\":\"data\",\"data\":\"not base64!\"}");
  auto bad_reader = make_sftp_stream_reader(bad_channel);
  expect_transfer_error([&] { char block[8]; (void)bad_reader->read(block, sizeof(block)); },
                        TransferErrorCode::Remote,
                        "an invalid data chunk must be rejected");

  auto unexpected_channel = std::make_shared<FakeStreamChannel>();
  unexpected_channel->enqueue("{\"type\":\"ready\"}");
  unexpected_channel->enqueue("{\"type\":\"data\",\"data\":\"aGVsbG8=\"}");
  unexpected_channel->enqueue("{\"type\":\"nonsense\"}");
  auto unexpected_reader = make_sftp_stream_reader(unexpected_channel);
  char block[8];
  check(unexpected_reader->read(block, sizeof(block)) == 5,
        "the SFTP reader must return a valid small data chunk");
  expect_transfer_error([&] { (void)unexpected_reader->read(block, sizeof(block)); },
                        TransferErrorCode::Remote,
                        "an unexpected download event must fail the read");

  auto eof_channel = std::make_shared<FakeStreamChannel>();
  eof_channel->enqueue("{\"type\":\"ready\"}");
  auto eof_reader = make_sftp_stream_reader(eof_channel);
  expect_transfer_error([&] { char block2[8]; (void)eof_reader->read(block2, sizeof(block2)); },
                        TransferErrorCode::Remote,
                        "a download without a final event must fail");

  auto exit_channel = std::make_shared<FakeStreamChannel>();
  exit_channel->enqueue("{\"type\":\"ready\"}");
  exit_channel->enqueue("{\"type\":\"closed\",\"exit_code\":0}");
  exit_channel->exit_code = 1;
  auto exit_reader = make_sftp_stream_reader(exit_channel);
  expect_transfer_error([&] { char block3[8]; (void)exit_reader->read(block3, sizeof(block3)); },
                        TransferErrorCode::Remote,
                        "a nonzero worker exit must fail the download");

  auto no_ready = std::make_shared<FakeStreamChannel>();
  no_ready->enqueue("{\"type\":\"error\",\"code\":\"sftp_error\",\"message\":\"no server\"}");
  expect_transfer_error([&] { (void)make_sftp_stream_reader(no_ready); },
                        TransferErrorCode::Remote,
                        "a failed ready handshake must be reported");
}

void run_sftp_stream_writer_checks() {
  constexpr std::size_t kChunkBytes = 32 * 1024;

  auto channel = std::make_shared<FakeStreamChannel>();
  channel->enqueue("{\"type\":\"ready\"}");
  channel->enqueue("{\"type\":\"closed\",\"exit_code\":0,\"bytes\":5}");
  auto writer = make_sftp_stream_writer(channel);
  writer->write("hello", 5);
  check(channel->written_.size() == 1 &&
            channel->written_[0] == "{\"data\":\"aGVsbG8=\",\"type\":\"data\"}",
        "a small upload write must be one exact data frame");
  writer->finish();
  check(channel->written_.size() == 2 &&
            channel->written_.back() == "{\"type\":\"close\"}" && channel->write_closed,
        "finish must send the close frame and close the worker input");

  // A write larger than one chunk is split into bounded frames.
  auto split_channel = std::make_shared<FakeStreamChannel>();
  split_channel->enqueue("{\"type\":\"ready\"}");
  split_channel->enqueue("{\"type\":\"closed\",\"exit_code\":0}");
  auto split_writer = make_sftp_stream_writer(split_channel);
  const std::string payload(kChunkBytes + 5, 'x');
  split_writer->write(payload.data(), payload.size());
  check(split_channel->written_.size() == 2,
        "an oversized upload write must split into one frame per 32 KiB chunk");
  const auto first = nlohmann::json::parse(split_channel->written_[0]);
  const auto second = nlohmann::json::parse(split_channel->written_[1]);
  check(first.at("type") == "data" && second.at("type") == "data" &&
            first.at("data").get<std::string>().size() == ((kChunkBytes + 2) / 3) * 4 &&
            second.at("data").get<std::string>().size() == ((5 + 2) / 3) * 4,
        "each upload frame must encode at most one 32 KiB chunk");
  split_writer->finish();

  auto error_channel = std::make_shared<FakeStreamChannel>();
  error_channel->enqueue("{\"type\":\"ready\"}");
  error_channel->enqueue("{\"type\":\"error\",\"code\":\"sftp_error\",\"message\":\"no\"}");
  auto error_writer = make_sftp_stream_writer(error_channel);
  expect_transfer_error([&] { error_writer->finish(); }, TransferErrorCode::Remote,
                        "an upload error frame must fail finish");

  auto eof_channel = std::make_shared<FakeStreamChannel>();
  eof_channel->enqueue("{\"type\":\"ready\"}");
  auto eof_writer = make_sftp_stream_writer(eof_channel);
  expect_transfer_error([&] { eof_writer->finish(); }, TransferErrorCode::Remote,
                        "a missing upload completion frame must fail finish");
}

}  // namespace

int main() {
  // Enum contracts and round trips.
  check(to_string(TransferDirection::Upload) == "upload" &&
            to_string(TransferDirection::Download) == "download",
        "transfer direction strings must match the shared contract");
  check(to_string(TransferStatus::WaitingConflict) == "waitingConflict" &&
            to_string(TransferStatus::Interrupted) == "interrupted",
        "transfer status strings must match the shared contract");
  check(to_string(ConflictPolicy::Ask) == "ask" &&
            to_string(ConflictActionKind::Rename) == "rename" &&
            to_string(TransferFailureKind::RemoteIo) == "remoteIo",
        "conflict policy/action and failure kind strings must match the contract");
  check(parse_transfer_status("partial") == TransferStatus::Partial &&
            parse_transfer_status("waitingConflict") == TransferStatus::WaitingConflict,
        "known status strings must parse");
  check(!parse_transfer_status("flying").has_value() &&
            !parse_transfer_direction("sideways").has_value() &&
            !parse_conflict_policy("maybe").has_value() &&
            !parse_transfer_failure_kind("nope").has_value(),
        "unknown enum strings must not parse");
  check(is_terminal(TransferStatus::Completed) && is_terminal(TransferStatus::Interrupted) &&
            !is_terminal(TransferStatus::Running) && !is_terminal(TransferStatus::WaitingConflict),
        "terminal statuses must match the shared contract");

  // A fresh snapshot is a queued record with equal start/update timestamps.
  const auto fresh = new_transfer_snapshot("tr_fresh", "srv_01abc", TransferDirection::Download);
  check(fresh.transfer_id == "tr_fresh" && fresh.server_id == "srv_01abc" &&
            fresh.direction == TransferDirection::Download &&
            fresh.status == TransferStatus::Queued &&
            fresh.started_at_epoch_ms == fresh.updated_at_epoch_ms &&
            fresh.started_at_epoch_ms > 0 && !fresh.total_files.has_value() &&
            fresh.completed_files == 0 && !fresh.active_conflict.has_value() &&
            fresh.failures.empty() && fresh.staging_residue.empty(),
        "a new transfer snapshot must start queued with zero progress");

  // The IPC snapshot is the exact 19-field contract with nullable fields.
  TransferSnapshot snapshot = fresh;
  snapshot.status = TransferStatus::WaitingConflict;
  snapshot.total_files = 2;
  snapshot.completed_files = 1;
  snapshot.skipped_files = 0;
  snapshot.total_bytes = 30;
  snapshot.transferred_bytes = 20;
  snapshot.current_item = "logs/app.log";
  snapshot.current_item_bytes = 5;
  snapshot.current_item_total_bytes = 10;
  snapshot.verified_files = 1;
  snapshot.unverified_files = 0;
  ConflictRequest request;
  request.item_index = 1;
  request.source_name = "app.log";
  request.target_name = "app.log";
  request.existing_size = 10;
  request.incoming_size = 10;
  request.existing_modified_epoch_seconds = 1700000000;
  request.allowed_actions = {ConflictActionKind::Skip, ConflictActionKind::Overwrite,
                             ConflictActionKind::Rename};
  snapshot.active_conflict = request;
  TransferItemFailure failure;
  failure.item_index = 0;
  failure.item = "old.log";
  failure.kind = TransferFailureKind::RemoteIo;
  failure.message = "source does not exist";
  failure.staging_residue = "old.log.yukinal-tr-0-1.part";
  snapshot.failures.push_back(failure);
  snapshot.staging_residue.push_back("old.log.yukinal-tr-0-1.part");

  const auto encoded = transfer_snapshot_json(snapshot);
  check(encoded.is_object() && encoded.size() == 19,
        "the snapshot must serialize exactly the 19 contract fields");
  const nlohmann::json expected_failure = {
      {"item", "old.log"},
      {"kind", "remoteIo"},
      {"message", "source does not exist"},
      {"itemIndex", 0},
      {"stagingResidue", "old.log.yukinal-tr-0-1.part"},
  };
  const nlohmann::json expected = {
      {"transferId", "tr_fresh"},
      {"serverId", "srv_01abc"},
      {"direction", "download"},
      {"status", "waitingConflict"},
      {"startedAtEpochMs", fresh.started_at_epoch_ms},
      {"updatedAtEpochMs", fresh.updated_at_epoch_ms},
      {"totalFiles", 2},
      {"completedFiles", 1},
      {"skippedFiles", 0},
      {"totalBytes", 30},
      {"transferredBytes", 20},
      {"currentItem", "logs/app.log"},
      {"currentItemBytes", 5},
      {"currentItemTotalBytes", 10},
      {"activeConflict",
       {{"itemIndex", 1},
        {"sourceName", "app.log"},
        {"targetName", "app.log"},
        {"existingSize", 10},
        {"incomingSize", 10},
        {"existingModifiedEpochSeconds", 1700000000},
        {"allowedActions", nlohmann::json::array({"skip", "overwrite", "rename"})}}},
      {"verifiedFiles", 1},
      {"unverifiedFiles", 0},
      {"failures", nlohmann::json::array({expected_failure})},
      {"stagingResidue", nlohmann::json::array({"old.log.yukinal-tr-0-1.part"})},
  };
  check(encoded == expected, "the snapshot JSON must match the shared contract exactly");

  // Absent optional failure fields must be omitted, not emitted as null, so the
  // strictObject Zod parse accepts them.
  TransferSnapshot sparse = fresh;
  TransferItemFailure bare;
  bare.item = "directory";
  bare.kind = TransferFailureKind::InvalidInput;
  bare.message = "special files cannot be transferred";
  sparse.failures.push_back(bare);
  sparse.active_conflict = std::nullopt;
  const auto sparse_failure = transfer_snapshot_json(sparse).at("failures").at(0);
  check(!sparse_failure.contains("itemIndex") && !sparse_failure.contains("stagingResidue") &&
            sparse_failure.size() == 3,
        "an optional failure index/residue must be omitted when absent");
  const auto sparse_json = transfer_snapshot_json(sparse);
  check(sparse_json.at("activeConflict").is_null() &&
            sparse_json.at("totalFiles").is_null() &&
            sparse_json.at("currentItem").is_null(),
        "nullable progress fields must serialize as null");

  // A persisted snapshot must reconstruct exactly, so durable history survives a
  // restart without losing the conflict, the failures or the staging residue.
  const auto stored = transfer_snapshot_from_json(encoded);
  check(transfer_snapshot_json(stored) == encoded,
        "a stored snapshot must round trip through the parser");
  check(stored.transfer_id == snapshot.transfer_id &&
            stored.server_id == snapshot.server_id &&
            stored.direction == snapshot.direction && stored.status == snapshot.status &&
            stored.started_at_epoch_ms == snapshot.started_at_epoch_ms &&
            stored.total_files == snapshot.total_files &&
            stored.current_item == snapshot.current_item,
        "a parsed snapshot must carry the contract identifiers and progress");
  check(stored.active_conflict.has_value() &&
            stored.active_conflict->item_index == 1 &&
            stored.active_conflict->existing_modified_epoch_seconds == 1700000000 &&
            stored.active_conflict->allowed_actions.size() == 3 &&
            stored.active_conflict->allowed_actions[2] == ConflictActionKind::Rename,
        "a parsed snapshot must carry its active conflict");
  check(stored.failures.size() == 1 && stored.failures[0].item == "old.log" &&
            stored.failures[0].item_index.has_value() && *stored.failures[0].item_index == 0 &&
            stored.failures[0].kind == TransferFailureKind::RemoteIo &&
            stored.failures[0].staging_residue.has_value() && stored.staging_residue.size() == 1,
        "a parsed snapshot must carry its failures and staging residue");
  const auto sparse_stored = transfer_snapshot_from_json(sparse_json);
  check(transfer_snapshot_json(sparse_stored) == sparse_json &&
            !sparse_stored.active_conflict.has_value() &&
            !sparse_stored.failures[0].item_index.has_value() &&
            !sparse_stored.failures[0].staging_residue.has_value(),
        "an omitted optional field must parse back as absent");

  // A malformed persisted row must be reported loudly, never silently dropped.
  expect_runtime_error([] { (void)transfer_snapshot_from_json(nlohmann::json::array()); },
                       "a non-object snapshot must be rejected");
  expect_runtime_error([] { (void)transfer_snapshot_from_json(nlohmann::json::object()); },
                       "a snapshot missing every field must be rejected");
  {
    auto broken = encoded;
    broken["status"] = "flying";
    expect_runtime_error([&] { (void)transfer_snapshot_from_json(broken); },
                         "a snapshot with an unknown status must be rejected");
  }
  {
    auto broken = encoded;
    broken["transferredBytes"] = -1;
    expect_runtime_error([&] { (void)transfer_snapshot_from_json(broken); },
                         "a snapshot with a negative counter must be rejected");
  }
  {
    auto broken = encoded;
    broken["failures"] = nlohmann::json::object();
    expect_runtime_error([&] { (void)transfer_snapshot_from_json(broken); },
                         "a snapshot whose failures are not an array must be rejected");
  }
  {
    auto broken = encoded;
    broken["activeConflict"]["allowedActions"] = nlohmann::json::array({"explode"});
    expect_runtime_error([&] { (void)transfer_snapshot_from_json(broken); },
                         "a snapshot with an unknown conflict action must be rejected");
  }
  {
    auto broken = encoded;
    broken["stagingResidue"] = nlohmann::json::array({1});
    expect_runtime_error([&] { (void)transfer_snapshot_from_json(broken); },
                         "a snapshot with a non-string residue entry must be rejected");
  }

  // Host-private staging inventory round trips for both variants.
  TransferStagingFile local;
  local.path = "C:\\stage\\file.part";
  local.item_index = 3;
  TransferStagingFile remote;
  remote.remote = true;
  remote.server_id = "srv_01abc";
  remote.path = "/srv/tmp/file.part";
  remote.item_index = 4;
  const auto staging = transfer_staging_json({local, remote});
  check(staging.size() == 2 && staging[0].at("location") == "local" &&
            staging[0].at("path") == "C:\\stage\\file.part" &&
            !staging[0].contains("serverId") && staging[1].at("location") == "remote" &&
            staging[1].at("serverId") == "srv_01abc" && staging[1].at("itemIndex") == 4,
        "staging files must encode the tagged location variant");
  const auto parsed = transfer_staging_from_json(staging);
  check(parsed.has_value() && parsed->size() == 2 && (*parsed)[0].path == local.path &&
            !(*parsed)[0].remote && (*parsed)[1].remote &&
            (*parsed)[1].server_id == "srv_01abc" && (*parsed)[1].item_index == 4,
        "staging files must round trip through JSON");
  check(!transfer_staging_from_json(nlohmann::json::object()).has_value() &&
            !transfer_staging_from_json(
                 nlohmann::json::array({{{"location", "remote"}, {"path", "/x"}}}))
                 .has_value(),
        "malformed staging entries must be rejected");

  // Conflict action parsing.
  check(conflict_action_from_json({{"action", "skip"}}).kind == ConflictActionKind::Skip &&
            conflict_action_from_json({{"action", "overwrite"}}).kind ==
                ConflictActionKind::Overwrite,
        "skip/overwrite conflict actions must parse");
  const auto rename = conflict_action_from_json({{"action", "rename"}, {"name", "new name.txt"}});
  check(rename.kind == ConflictActionKind::Rename && rename.rename_to == "new name.txt",
        "a rename conflict action must carry its name");
  expect_invalid([] { (void)conflict_action_from_json({{"action", "explode"}}); },
                 "an unknown conflict action must be rejected");
  expect_invalid([] { (void)conflict_action_from_json({{"action", "rename"}}); },
                 "a rename without a name must be rejected");
  expect_invalid([] { (void)conflict_action_from_json({{"action", "rename"}, {"name", "a/b"}}); },
                 "a rename with a separator must be rejected");

  // Safe file names.
  for (const char* accepted : {"report.txt", "a b.pdf", ".hidden", "na\u00efve.txt"}) {
    validate_safe_name(accepted);
  }
  for (const char* rejected : {"", ".", "..", "a/b", "a\\b", "a:b", "trailing ", "trailing.",
                               "line\nfeed", "CON", "con.txt", "NUL.log"}) {
    expect_invalid([rejected] { validate_safe_name(rejected); },
                   "an unsafe file name must be rejected");
  }

  // Remote paths.
  for (const char* accepted : {"/", "/srv", "/srv/api/app.log", "/srv/api/"}) {
    validate_transfer_remote_path(accepted);
  }
  for (const char* rejected : {"", "relative", "/srv/../etc", "/srv//api", "/srv\\api",
                               "/srv/app\n.log"}) {
    expect_invalid([rejected] { validate_transfer_remote_path(rejected); },
                   "an invalid remote path must be rejected");
  }
  validate_transfer_remote_path("/" + std::string(4095, 'a'));
  expect_invalid([] { validate_transfer_remote_path("/" + std::string(4096, 'a')); },
                 "a remote path longer than 4096 characters must be rejected");

  check(remote_child("/", "app.log") == "/app.log" &&
            remote_child("/srv", "app.log") == "/srv/app.log" &&
            remote_child("/srv/", "app.log") == "/srv/app.log",
        "remote_child must join paths without doubling separators");
  expect_invalid([] { (void)remote_child("/srv", ".."); },
                 "remote_child must reject a traversal name");

  // Staging ownership marker.
  check(is_owned_staging_name("report.yukinal-abcdef-5-3.part", "abc-def") &&
            !is_owned_staging_name("report.yukinal-abcdee-5-3.part", "abc-def") &&
            !is_owned_staging_name("report.yukinal-abcdef-5-3.txt", "abc-def") &&
            !is_owned_staging_name("report.yukinal-abcdef-x-3.part", "abc-def") &&
            !is_owned_staging_name("report.yukinal-abcdef-5-3-4.part", "abc-def") &&
            !is_owned_staging_name("report.part", "abc-def"),
        "staging ownership must require the transfer-scoped basename");

  // Generated transfer ids are unique and match the retired format.
  std::set<std::string> ids;
  for (int index = 0; index < 32; ++index) ids.insert(new_transfer_id());
  check(ids.size() == 32, "generated transfer ids must be unique");
  const auto parts = split(*ids.begin(), '_');
  check(parts.size() == 4 && parts[0] == "tr" && all_digits(parts[3]),
        "a generated transfer id must follow tr_<pid>_<nanos>_<seq>");

  run_local_checks();
  run_sftp_backend_checks();
  run_sftp_stream_reader_checks();
  run_sftp_stream_writer_checks();

  std::cout << "transfer contract tests: green\n";
  return 0;
}

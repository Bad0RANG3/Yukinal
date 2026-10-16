// Unit tests for the pure transfer contract module (`transfer_types`). These
// cover the shared `TransferSnapshot` JSON shape, the host-private staging
// encoding and the path/name helpers the transfer manager relies on. They never
// touch the database or the network.

#include "yukinal/desktop_host/transfer_types.hpp"

#include <algorithm>
#include <cstdlib>
#include <iostream>
#include <set>
#include <string>
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

  std::cout << "transfer contract tests: green\n";
  return 0;
}

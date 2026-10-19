#include "test_support.hpp"

#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <winsock2.h>
#include <ws2tcpip.h>
#else
#include <netdb.h>
#endif

#include "exit_status.hpp"
#include "ssh_network.hpp"
#include "yukinal/ssh.hpp"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstddef>
#include <filesystem>
#include <fstream>
#include <memory>
#include <sstream>
#include <stdexcept>
#include <string>
#include <system_error>
#include <thread>

using yukinal::ssh_service::detail::AddressResolution;
using yukinal::ssh_service::detail::interpret_exit_status;
using yukinal::ssh_service::detail::resolve_address_list;

namespace {

bool is_absolute_path(const std::string& path) {
  if (path.empty()) return false;
  if (path[0] == '/' || path[0] == '\\') return true;
  // A Windows drive prefix, e.g. `D:`. A bare relative path cannot match.
  return path.size() > 2 && path[1] == ':';
}

/// Resolve one of the service sources relative to this test file's directory.
///
/// The seam and guardrail cases below live in this translation unit because it
/// is the only network-flavoured source registered in `native/CMakeLists.txt`;
/// the test target list is outside the write scope of the exit-status fix, so a
/// dedicated `test_exit_status.cpp` cannot be registered from here.
///
/// `__FILE__` is absolute for this target -- `build.make` compiles it as
/// `-c D:\yukinal\native\tests\test_network.cpp` -- so the result does not depend
/// on the process working directory. That property is asserted rather than
/// assumed, because a relative path here would make every scan below
/// cwd-dependent and silently wrong when ctest runs from the build directory.
std::string service_source_path(const char* path_relative_to_native) {
  std::string path = __FILE__;
  for (int level = 0; level < 2; ++level) {
    const auto separator = path.find_last_of("/\\");
    if (separator == std::string::npos || separator == 0) {
      throw std::runtime_error(std::string("unexpected test source path: ") + __FILE__);
    }
    path.erase(separator);
  }
  if (!is_absolute_path(path)) {
    throw std::runtime_error("the test source path is not absolute: " + path);
  }
  path += "/";
  path += path_relative_to_native;
  return path;
}

std::string read_service_source(const char* path_relative_to_native) {
  const std::string path = service_source_path(path_relative_to_native);
  std::ifstream input(path, std::ios::in | std::ios::binary);
  if (!input) throw std::runtime_error("could not read service source " + path);
  std::ostringstream contents;
  contents << input.rdbuf();
  return contents.str();
}

/// Non-empty-match guard for the source scans below.
///
/// Every one of these cases asserts on *occurrence counts*, and several of them
/// assert `== 0`. A path that resolves to the wrong file -- or a file that was
/// emptied/moved -- therefore produces the exact value that means "pass": zero
/// hits. `read_service_source` already throws on a missing file, but not on a
/// path that lands on some other, shorter file, so pin a size floor and a
/// landmark before trusting any count. Without this the family fails open.
void require_scannable_source(const std::string& source, const char* landmark) {
  // The smallest file scanned here is `src/main.cpp` at ~5.5 kB; the floor is
  // deliberately an order of magnitude below that so it only trips on a
  // genuinely wrong target, not on incidental trimming.
  YUKINAL_REQUIRE(source.size() > 1000);
  YUKINAL_REQUIRE(source.find(landmark) != std::string::npos);
}

std::size_t count_occurrences(const std::string& haystack, const std::string& needle) {
  std::size_t count = 0;
  for (std::size_t at = haystack.find(needle); at != std::string::npos;
       at = haystack.find(needle, at + needle.size())) {
    ++count;
  }
  return count;
}

/// 1-based line number of the first `needle`, or 0 when absent.
///
/// Placement assertions are easier to read -- and to check against the source
/// in a review -- as line numbers than as byte offsets, which is also how the
/// task states the requirement ("the guard's line is after `int main(` and
/// before the first `std::thread`").
std::size_t line_of(const std::string& source, const std::string& needle) {
  const auto at = source.find(needle);
  if (at == std::string::npos) return 0;
  return 1 + static_cast<std::size_t>(std::count(source.begin(), source.begin() + at, '\n'));
}

/// Total number of newlines before `offset`, i.e. its 1-based line number.
std::size_t line_at_offset(const std::string& source, std::size_t offset) {
  if (offset > source.size()) offset = source.size();
  std::size_t lines = 1;
  for (std::size_t at = 0; at < offset; ++at) {
    if (source[at] == '\n') ++lines;
  }
  return lines;
}

/// Number of lines in `source`; `line_of` never exceeds this.
std::size_t line_count(const std::string& source) {
  return 1 + static_cast<std::size_t>(std::count(source.begin(), source.end(), '\n'));
}

}  // namespace

YUKINAL_TEST(probe_only_target_rejects_all_authenticated_operations) {
  yukinal::ssh_service::TargetConfig target;
  target.target_id = "probe-target";
  target.host = "example.test";
  target.port = 22;
  target.username = "ssh_probe";
  target.probe_only = true;

  yukinal::ssh_service::ExecRequest request;
  request.operation = "service_status";
  request.service = "nginx.service";
  request.timeout_seconds = 5;
  const auto result = yukinal::ssh_service::run_operation(target, request);

  YUKINAL_REQUIRE(!result.completed);
  YUKINAL_REQUIRE(result.error_code == "operation_not_allowed");
}

YUKINAL_TEST(host_key_probe_rejects_regular_authenticated_target) {
  yukinal::ssh_service::TargetConfig target;
  target.target_id = "regular-target";
  target.host = "example.test";
  target.port = 22;
  target.username = "ops";

  yukinal::ssh_service::ExecRequest request;
  request.operation = "host_key_probe";
  request.timeout_seconds = 5;
  const auto result = yukinal::ssh_service::run_operation(target, request);

  YUKINAL_REQUIRE(!result.completed);
  YUKINAL_REQUIRE(result.error_code == "operation_not_allowed");
}

YUKINAL_TEST(dns_resolution_obeys_the_operation_deadline) {
  using Clock = std::chrono::steady_clock;
  auto calls = std::make_shared<std::atomic_int>(0);
  const yukinal::ssh_service::detail::GetAddrInfo slow_resolver =
      [calls](const char*, const char*, const addrinfo*, addrinfo**) {
        if (calls->fetch_add(1) == 0) return EAI_NONAME;
        std::this_thread::sleep_for(std::chrono::milliseconds(500));
        return EAI_NONAME;
      };

  addrinfo hints{};
  hints.ai_family = AF_UNSPEC;
  hints.ai_socktype = SOCK_STREAM;
  addrinfo* addresses = nullptr;
  const auto started = Clock::now();
  const auto status = resolve_address_list("slow.invalid", "22", hints,
      started + std::chrono::milliseconds(25), &addresses, slow_resolver);
  const auto elapsed = std::chrono::duration_cast<std::chrono::milliseconds>(Clock::now() - started);

  YUKINAL_REQUIRE(status == AddressResolution::timed_out);
  YUKINAL_REQUIRE(addresses == nullptr);
  YUKINAL_REQUIRE(elapsed < std::chrono::milliseconds(200));

  // Let the deliberately slow resolver finish so the detached test worker is
  // quiescent before the test executable exits.
  std::this_thread::sleep_for(std::chrono::milliseconds(520));
}

// Regression guard for H-1: before the fix `ssh.cpp` read only
// `libssh2_channel_get_exit_status`, which returns the zero-seeded channel field
// and therefore reported "the server closed without an exit-status request" as a
// successful `exit 0`. `interpret_exit_status` is the seam that keeps those two
// cases apart; `has_exit_status == 0` is the "not received" signal that the
// pre-fix code could not express at all.
YUKINAL_TEST(interpret_exit_status_separates_absent_status_from_exit_zero) {
  const auto absent = interpret_exit_status(/*has_exit_status=*/0, /*status=*/0);
  YUKINAL_REQUIRE(!absent.received);
  YUKINAL_REQUIRE(absent.value == 0);

  // A server that never sent "exit-status" leaves libssh2's field at its
  // SSH2_CALLOC zero seed even when the channel produced output, so a non-zero
  // seed must not be mistaken for a received status either.
  const auto absent_with_seed = interpret_exit_status(/*has_exit_status=*/0, /*status=*/7);
  YUKINAL_REQUIRE(!absent_with_seed.received);

  const auto clean = interpret_exit_status(/*has_exit_status=*/1, /*status=*/0);
  YUKINAL_REQUIRE(clean.received);
  YUKINAL_REQUIRE(clean.value == 0);

  const auto failing = interpret_exit_status(/*has_exit_status=*/1, /*status=*/3);
  YUKINAL_REQUIRE(failing.received);
  YUKINAL_REQUIRE(failing.value == 3);
}

// RFC 4254 carries the exit status as a `uint32`; libssh2 narrows it to a signed
// `int` with no range check. A received status is therefore reported verbatim
// even when the high bit makes it negative: the remote result *is* known, so the
// service must not downgrade it to `result_unknown` (which now means strictly
// "no exit-status request arrived"). Consumers that need the POSIX 0..255 range
// (`agent/src/yukinal_ssh/client.py` server_exec, `ssh_tools.py`) still validate
// it themselves.
YUKINAL_TEST(interpret_exit_status_preserves_large_unsigned_statuses) {
  constexpr int kStatusFrom0x80000000 = static_cast<int>(0x80000000u);
  YUKINAL_REQUIRE(kStatusFrom0x80000000 < 0);

  const auto narrowed = interpret_exit_status(/*has_exit_status=*/1, kStatusFrom0x80000000);
  YUKINAL_REQUIRE(narrowed.received);
  YUKINAL_REQUIRE(narrowed.value == kStatusFrom0x80000000);

  const auto negative = interpret_exit_status(/*has_exit_status=*/1, /*status=*/-1);
  YUKINAL_REQUIRE(negative.received);
  YUKINAL_REQUIRE(negative.value == -1);

  const auto boundary = interpret_exit_status(/*has_exit_status=*/1, /*status=*/255);
  YUKINAL_REQUIRE(boundary.received);
  YUKINAL_REQUIRE(boundary.value == 255);
}

// Guardrail for H-2. This host is Windows, which has no SIGPIPE, so this case is
// deliberately a *source-level* assertion rather than a behavior test: it proves
// the non-Windows entry point installs the disposition before any socket or TLS
// work, and nothing more. Confirming that a Linux RST actually becomes EPIPE
// instead of killing the process needs a Linux runner (see the report's
// "unfixed parts").
//
// What this does NOT prove: that the kernel really turns an RST into EPIPE, that
// the disposition survives every later library call, or that no library resets
// it. It only fails closed on "someone deleted the guard", "someone moved it
// after the dispatch below" or "someone reintroduced a SIG_DFL/sigaction reset".
YUKINAL_TEST(non_windows_entry_point_ignores_sigpipe_before_startup) {
  const std::string source = read_service_source("src/main.cpp");
  // Non-empty-match guard: several counts below are `== 0`, and a path that
  // resolves to the wrong (or an empty) file would satisfy exactly those.
  require_scannable_source(source, "int main(");

  YUKINAL_REQUIRE(count_occurrences(source, "#include <csignal>") == 1);
  YUKINAL_REQUIRE(count_occurrences(source, "std::signal(SIGPIPE, SIG_IGN)") == 1);
  // `#ifndef _WIN32` appears once, so a guard placed outside it (or a second,
  // unguarded copy) is a hard failure rather than a silent extra occurrence.
  YUKINAL_REQUIRE(count_occurrences(source, "#ifndef _WIN32") == 1);
  YUKINAL_REQUIRE(count_occurrences(source, "int main(") == 1);
  // Nothing in the entry point undoes it again.
  YUKINAL_REQUIRE(count_occurrences(source, "SIG_DFL") == 0);
  YUKINAL_REQUIRE(count_occurrences(source, "sigaction") == 0);
  YUKINAL_REQUIRE(count_occurrences(source, "siginterrupt") == 0);

  // Placement, stated as line numbers because that is how it is reviewed: the
  // guard sits inside `main` (not at file scope, not at byte 0) and after the
  // entry point opens.
  const std::size_t include_line = line_of(source, "#include <csignal>");
  const std::size_t guard_line = line_of(source, "std::signal(SIGPIPE, SIG_IGN)");
  const std::size_t entry_line = line_of(source, "int main(");
  YUKINAL_REQUIRE(include_line == 1);
  YUKINAL_REQUIRE(guard_line > entry_line);
  YUKINAL_REQUIRE(guard_line < line_count(source));

  // Inside the `#ifndef _WIN32` branch, so a Windows build never sees the call:
  // the guard is after the `#ifndef` and before the `#endif` that closes it.
  const std::size_t ifndef_line = line_of(source, "#ifndef _WIN32");
  YUKINAL_REQUIRE(ifndef_line > 0);
  YUKINAL_REQUIRE(ifndef_line < guard_line);
  const auto guard_at = source.find("std::signal(SIGPIPE, SIG_IGN)");
  const auto endif_at = source.find("#endif", guard_at);
  YUKINAL_REQUIRE(endif_at != std::string::npos);
  YUKINAL_REQUIRE(line_at_offset(source, guard_at) < line_at_offset(source, endif_at));

  const std::size_t thread_line = line_of(source, "std::thread");
  if (thread_line != 0) YUKINAL_REQUIRE(guard_line < thread_line);
}

// The paths the guardrail cases scan must not depend on the process working
// directory: ctest runs this binary from `native/build-nmake`, so a relative
// resolution would either throw or -- worse -- land on some other file and make
// every count below meaningless. `__FILE__` is absolute for this target
// (build.make compiles `-c D:\yukinal\native\tests\test_network.cpp`); assert
// that rather than assume it, and prove the read survives a cwd change.
YUKINAL_TEST(service_source_paths_do_not_depend_on_the_working_directory) {
  const std::string expected = service_source_path("src/main.cpp");
  YUKINAL_REQUIRE(is_absolute_path(expected));
  YUKINAL_REQUIRE(std::filesystem::is_regular_file(expected));

  const auto original_cwd = std::filesystem::current_path();
  struct RestoreWorkingDirectory {
    std::filesystem::path path;
    ~RestoreWorkingDirectory() {
      std::error_code ignored;
      std::filesystem::current_path(path, ignored);
    }
  } restore{original_cwd};

  std::error_code error;
  std::filesystem::current_path(std::filesystem::temp_directory_path(), error);
  YUKINAL_REQUIRE(!error);
  YUKINAL_REQUIRE(std::filesystem::current_path() != original_cwd);

  // Same absolute path, and still readable, after the cwd moved somewhere
  // unrelated: this is what a relative scan would fail.
  YUKINAL_REQUIRE(service_source_path("src/main.cpp") == expected);
  require_scannable_source(read_service_source("src/main.cpp"), "int main(");
}

// H-1 fixes two independent call sites. A seam unit test cannot notice if one
// of them is reverted to the bare numeric read, so pin the source shape: every
// numeric read must be paired with the `has_exit_status` flag, and there must be
// exactly the two known pairs (exec and terminal).
//
// Still structural, not behavioral: it reads text. The *behavioral* reader for
// the terminal frame lives in `scripts/test_ssh_mock.py` (the
// `server_exit_status=True` case asserts the `closed` frame carries
// `0x80000000` verbatim), which `ctest -R ssh_mock_integration` runs against the
// real service binary. This case exists so the cheap `yukinal_ssh_tests` target
// also fails closed when the terminal frame is reverted -- mutation M4 reverted
// only that frame and left this target fully green before these assertions.
YUKINAL_TEST(both_exit_status_call_sites_consult_has_exit_status) {
  const std::string source = read_service_source("src/ssh.cpp");
  require_scannable_source(source, "int run_terminal_session(");

  YUKINAL_REQUIRE(count_occurrences(source, "libssh2_channel_has_exit_status(") == 2);
  YUKINAL_REQUIRE(count_occurrences(source, "detail::interpret_exit_status(") == 2);
  YUKINAL_REQUIRE(count_occurrences(source, "libssh2_channel_get_exit_status(") == 2);
  // The pre-fix `if (result.exit_code < 0)` guard must be gone; `result_unknown`
  // is now reserved for "no exit-status request arrived".
  YUKINAL_REQUIRE(count_occurrences(source, "result.exit_code < 0") == 0);
  YUKINAL_REQUIRE(count_occurrences(source, "#include \"exit_status.hpp\"") == 1);

  // The terminal `closed` frame must be gated by the received flag, not by the
  // sign of the value. `exit_code` starts at -1 and is only ever assigned from a
  // received status, so a sign test reports `null` for both "nothing arrived"
  // (correct) and "the peer sent 0x80000000, which narrowed negative" (wrong).
  // Matching on the frame's own key/value shape rather than on a bare identifier
  // keeps this tied to the frame the mutation actually touched.
  YUKINAL_REQUIRE(count_occurrences(source, "{\"exit_code\", exit_status_received") == 1);
  YUKINAL_REQUIRE(count_occurrences(source, "{\"exit_code\", exit_code < 0") == 0);
  YUKINAL_REQUIRE(count_occurrences(source, "exit_code < 0") == 0);
}

// The service's HTTP mode is protected only because cpp-httplib's `Server`
// constructor calls `signal(SIGPIPE, SIG_IGN)`; the `--terminal` and
// `--sftp-*` worker modes never construct a `Server`, so before this fix a
// reset connection could terminate the whole worker. Pin the entry-point
// placement so a later refactor cannot quietly drop the guard again.
YUKINAL_TEST(sigpipe_guard_precedes_worker_mode_dispatch) {
  const std::string source = read_service_source("src/main.cpp");
  require_scannable_source(source, "int main(");

  const auto guard = source.find("std::signal(SIGPIPE, SIG_IGN)");
  YUKINAL_REQUIRE(guard != std::string::npos);
  YUKINAL_REQUIRE(guard > 0);
  const auto worker_dispatch = source.find("run_terminal_session");
  YUKINAL_REQUIRE(worker_dispatch != std::string::npos);
  const auto sftp_dispatch = source.find("run_sftp_stream");
  YUKINAL_REQUIRE(sftp_dispatch != std::string::npos);
  const auto http_entry = source.find("ssh_service::serve(");
  YUKINAL_REQUIRE(http_entry != std::string::npos);

  YUKINAL_REQUIRE(guard < worker_dispatch);
  YUKINAL_REQUIRE(guard < sftp_dispatch);
  YUKINAL_REQUIRE(guard < http_entry);

  // The entry point itself creates no thread; the workers live behind these two
  // calls. If a thread is ever created directly in `main`, the guard must still
  // precede it -- asserted conditionally because the count is zero today, and an
  // unconditional `guard < npos` comparison would be trivially false.
  if (const auto first_thread = source.find("std::thread");
      first_thread != std::string::npos) {
    YUKINAL_REQUIRE(guard < first_thread);
  }
}

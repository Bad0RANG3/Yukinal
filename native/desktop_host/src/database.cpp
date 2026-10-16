#include "yukinal/desktop_host/database.hpp"

#include "schema_migrations.hpp"

#include <algorithm>
#include <array>
#include <chrono>
#include <cctype>
#include <cstdlib>
#include <cstdint>
#include <ctime>
#include <fstream>
#include <iomanip>
#include <initializer_list>
#include <iterator>
#include <limits>
#include <optional>
#include <set>
#include <sstream>
#include <stdexcept>
#include <string>
#include <utility>
#include <variant>
#include <vector>

#ifdef _WIN32
#include <windows.h>
#include <bcrypt.h>
#elif defined(__APPLE__)
#include <CommonCrypto/CommonDigest.h>
#else
#include <glib.h>
#endif

namespace yukinal::desktop_host {
namespace {

// A small, dependency-free SHA-256 (FIPS 180-4). The desktop host must compute the
// evidence content hash the same way every other Yukinal component does, and linking a
// platform crypto library here would duplicate what `host_key.cpp` does per platform.
class Sha256 {
 public:
  void update(const unsigned char* data, size_t length) {
    for (size_t i = 0; i < length; ++i) {
      buffer_[buffer_length_++] = data[i];
      if (buffer_length_ == 64) {
        transform();
        bit_length_ += 512;
        buffer_length_ = 0;
      }
    }
  }

  [[nodiscard]] std::string hexdigest() {
    const uint64_t bit_length = bit_length_ + static_cast<uint64_t>(buffer_length_) * 8;
    buffer_[buffer_length_++] = 0x80;
    if (buffer_length_ > 56) {
      while (buffer_length_ < 64) buffer_[buffer_length_++] = 0;
      transform();
      buffer_length_ = 0;
    }
    while (buffer_length_ < 56) buffer_[buffer_length_++] = 0;
    for (int i = 7; i >= 0; --i) {
      buffer_[buffer_length_++] = static_cast<unsigned char>((bit_length >> (i * 8)) & 0xff);
    }
    transform();
    static const char digits[] = "0123456789abcdef";
    std::string out;
    out.reserve(64);
    for (const uint32_t word : digest_) {
      for (int shift = 24; shift >= 0; shift -= 8) {
        const auto byte = static_cast<unsigned char>((word >> shift) & 0xff);
        out.push_back(digits[byte >> 4]);
        out.push_back(digits[byte & 0x0f]);
      }
    }
    return out;
  }

 private:
  static uint32_t rotr(uint32_t value, uint32_t bits) {
    return (value >> bits) | (value << (32 - bits));
  }

  void transform() {
    static const uint32_t k[64] = {
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2};
    uint32_t w[64];
    for (int i = 0; i < 16; ++i) {
      w[i] = (static_cast<uint32_t>(buffer_[i * 4]) << 24) |
             (static_cast<uint32_t>(buffer_[i * 4 + 1]) << 16) |
             (static_cast<uint32_t>(buffer_[i * 4 + 2]) << 8) |
             static_cast<uint32_t>(buffer_[i * 4 + 3]);
    }
    for (int i = 16; i < 64; ++i) {
      const uint32_t s0 = rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ (w[i - 15] >> 3);
      const uint32_t s1 = rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ (w[i - 2] >> 10);
      w[i] = w[i - 16] + s0 + w[i - 7] + s1;
    }
    uint32_t a = digest_[0], b = digest_[1], c = digest_[2], d = digest_[3];
    uint32_t e = digest_[4], f = digest_[5], g = digest_[6], h = digest_[7];
    for (int i = 0; i < 64; ++i) {
      const uint32_t s1 = rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25);
      const uint32_t ch = (e & f) ^ (~e & g);
      const uint32_t temp1 = h + s1 + ch + k[i] + w[i];
      const uint32_t s0 = rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22);
      const uint32_t maj = (a & b) ^ (a & c) ^ (b & c);
      const uint32_t temp2 = s0 + maj;
      h = g; g = f; f = e; e = d + temp1;
      d = c; c = b; b = a; a = temp1 + temp2;
    }
    digest_[0] += a; digest_[1] += b; digest_[2] += c; digest_[3] += d;
    digest_[4] += e; digest_[5] += f; digest_[6] += g; digest_[7] += h;
  }

  unsigned char buffer_[64] = {};
  size_t buffer_length_ = 0;
  uint64_t bit_length_ = 0;
  uint32_t digest_[8] = {0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                         0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19};
};

std::string sha256_hex(const std::string& input) {
  Sha256 hasher;
  hasher.update(reinterpret_cast<const unsigned char*>(input.data()), input.size());
  return hasher.hexdigest();
}

std::string path_as_utf8(const std::filesystem::path& path) {
  const auto encoded = path.u8string();
  return {encoded.begin(), encoded.end()};
}

const char* column_text(sqlite3_stmt* statement, int column) {
  const auto* value = sqlite3_column_text(statement, column);
  return value == nullptr ? "" : reinterpret_cast<const char*>(value);
}

nlohmann::json parse_json_column(sqlite3_stmt* statement, int column,
                                 const char* description) {
  try {
    return nlohmann::json::parse(column_text(statement, column));
  } catch (const nlohmann::json::exception&) {
    throw std::runtime_error(std::string("Stored server ") + description + " is invalid JSON");
  }
}

std::optional<nlohmann::json> parse_optional_json_column(sqlite3_stmt* statement, int column,
                                                          const char* description) {
  if (sqlite3_column_type(statement, column) == SQLITE_NULL) return std::nullopt;
  const auto* text = column_text(statement, column);
  if (*text == '\0') return std::nullopt;
  try {
    auto value = nlohmann::json::parse(text);
    if (value.is_null()) return std::nullopt;
    return value;
  } catch (const nlohmann::json::exception&) {
    throw std::runtime_error(std::string("Stored server ") + description + " is invalid JSON");
  }
}

void add_optional_text(nlohmann::json& object, const char* key,
                       sqlite3_stmt* statement, int column) {
  if (sqlite3_column_type(statement, column) != SQLITE_NULL) {
    object[key] = column_text(statement, column);
  }
}

bool is_one_of(const std::string& value, std::initializer_list<const char*> options) {
  return std::any_of(options.begin(), options.end(), [&value](const char* option) {
    return value == option;
  });
}

std::filesystem::path environment_path(const char* name) {
#ifdef _WIN32
  wchar_t buffer[32768];
  const auto length = GetEnvironmentVariableW(
      std::filesystem::path(name).wstring().c_str(), buffer,
      static_cast<DWORD>(std::size(buffer)));
  if (length == 0 || length >= std::size(buffer)) return {};
  return std::filesystem::path(std::wstring(buffer, length));
#else
  const auto* value = std::getenv(name);
  if (value == nullptr || *value == '\0') return {};
  return std::filesystem::u8path(value);
#endif
}

class Statement {
 public:
  Statement(sqlite3* connection, const char* sql) : connection_(connection) {
    const auto result = sqlite3_prepare_v2(connection, sql, -1, &statement_, nullptr);
    if (result != SQLITE_OK) {
      throw std::runtime_error(sqlite3_errmsg(connection));
    }
  }
  ~Statement() { sqlite3_finalize(statement_); }
  Statement(const Statement&) = delete;
  Statement& operator=(const Statement&) = delete;

  sqlite3_stmt* get() const { return statement_; }

 private:
  sqlite3* connection_;
  sqlite3_stmt* statement_ = nullptr;
};

void bind_text(sqlite3* connection, sqlite3_stmt* statement, int index,
               const std::string& value) {
  if (value.size() > static_cast<size_t>(std::numeric_limits<int>::max())) {
    throw std::runtime_error("A local database text value exceeds SQLite's size limit");
  }
  if (sqlite3_bind_text(statement, index, value.data(), static_cast<int>(value.size()),
                        SQLITE_TRANSIENT) != SQLITE_OK) {
    throw std::runtime_error(sqlite3_errmsg(connection));
  }
}

void bind_optional_text(sqlite3* connection, sqlite3_stmt* statement, int index,
                        const std::optional<std::string>& value) {
  if (!value.has_value()) {
    if (sqlite3_bind_null(statement, index) != SQLITE_OK) {
      throw std::runtime_error(sqlite3_errmsg(connection));
    }
    return;
  }
  bind_text(connection, statement, index, *value);
}

bool is_active_transfer_status(const std::string& status) {
  return is_one_of(status, {"queued", "running", "waitingConflict"});
}

bool is_known_transfer_status(const std::string& status) {
  return is_one_of(status, {"queued", "running", "waitingConflict", "completed", "partial",
                            "failed", "cancelled", "interrupted"});
}

std::string json_string_field(const nlohmann::json& value, const char* key) {
  if (!value.is_object()) return {};
  const auto found = value.find(key);
  if (found == value.end() || !found->is_string()) return {};
  return found->get<std::string>();
}

// Decodes one `file_transfers` row. A snapshot that still claims to be in
// progress cannot be live in this host (the C++ host owns no transfer manager),
// so the caller reconciles it to `interrupted` once the read statement is closed.
nlohmann::json decode_file_transfer(sqlite3_stmt* row) {
  const std::string transfer_id = column_text(row, 0);
  const std::string server_id = column_text(row, 1);
  const std::string direction = column_text(row, 2);
  const std::string status = column_text(row, 3);
  if (!is_one_of(direction, {"upload", "download"})) {
    throw std::runtime_error("Stored file transfer has an unknown direction");
  }
  if (!is_known_transfer_status(status)) {
    throw std::runtime_error("Stored file transfer has an unknown status");
  }
  nlohmann::json snapshot;
  try {
    snapshot = nlohmann::json::parse(column_text(row, 6));
  } catch (const nlohmann::json::exception&) {
    throw std::runtime_error("Stored file transfer snapshot is invalid JSON");
  }
  if (!snapshot.is_object() || json_string_field(snapshot, "transferId") != transfer_id ||
      json_string_field(snapshot, "serverId") != server_id ||
      json_string_field(snapshot, "direction") != direction ||
      json_string_field(snapshot, "status") != status) {
    throw std::runtime_error("Stored file transfer row does not match its snapshot");
  }
  return snapshot;
}

void persist_transfer_status(sqlite3* connection, const std::string& transfer_id,
                             const nlohmann::json& snapshot) {
  Statement update(connection,
                   "UPDATE file_transfers SET status = ?2, snapshot_json = ?3 "
                   "WHERE transfer_id = ?1");
  bind_text(connection, update.get(), 1, transfer_id);
  bind_text(connection, update.get(), 2, json_string_field(snapshot, "status"));
  bind_text(connection, update.get(), 3, snapshot.dump());
  if (sqlite3_step(update.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection));
  }
}

// Rewrites a still-active snapshot to `interrupted` and persists it, because no
// transfer can outlive the host process that owned it.
void reconcile_transfer(sqlite3* connection, nlohmann::json& snapshot) {
  if (!is_active_transfer_status(json_string_field(snapshot, "status"))) return;
  snapshot["status"] = "interrupted";
  persist_transfer_status(connection, json_string_field(snapshot, "transferId"), snapshot);
}

nlohmann::json parse_json_text(const std::string& text, const char* label) {
  try {
    return nlohmann::json::parse(text);
  } catch (const nlohmann::json::exception&) {
    throw std::runtime_error(std::string("Stored ") + label + " is invalid JSON");
  }
}

nlohmann::json mcp_json_array(sqlite3_stmt* row, int column, const char* label) {
  if (sqlite3_column_type(row, column) == SQLITE_NULL) return nlohmann::json::array();
  auto value = parse_json_text(column_text(row, column), label);
  if (!value.is_array()) throw std::runtime_error(std::string("Stored ") + label + " is invalid");
  return value;
}

// Decodes one `investigation_tasks` row into the shared `InvestigationTask` shape.
nlohmann::json decode_investigation_task(sqlite3_stmt* row) {
  nlohmann::json task = {
      {"id", column_text(row, 0)},
      {"objective", column_text(row, 3)},
      {"successCriteria", parse_json_text(column_text(row, 4), "investigation success criteria")},
      {"scope", parse_json_text(column_text(row, 5), "investigation scope")},
      {"guardrails", parse_json_text(column_text(row, 20), "investigation guardrails")},
      {"mode", column_text(row, 6)},
      {"permissionMode", column_text(row, 7)},
      {"automationLevel", column_text(row, 8)},
      {"createdBy", column_text(row, 13)},
      {"phase", column_text(row, 14)},
      {"status", column_text(row, 9)},
      {"budget",
       {{"maxSteps", sqlite3_column_int64(row, 10)},
        {"maxRunMs", sqlite3_column_int64(row, 11)},
        {"maxAttempts", sqlite3_column_int64(row, 12)}}},
      {"createdAt", column_text(row, 15)},
      {"updatedAt", column_text(row, 16)},
  };
  add_optional_text(task, "workspaceId", row, 1);
  add_optional_text(task, "serverId", row, 2);
  add_optional_text(task, "completedAt", row, 17);
  add_optional_text(task, "activeRunId", row, 18);
  if (sqlite3_column_type(row, 19) != SQLITE_NULL) {
    task["lastFailure"] = parse_json_text(column_text(row, 19), "investigation last failure");
  }
  return task;
}

nlohmann::json decode_investigation_evidence(sqlite3_stmt* row) {
  nlohmann::json evidence = {
      {"id", column_text(row, 0)},
      {"taskId", column_text(row, 1)},
      {"scope", parse_json_text(column_text(row, 2), "evidence scope")},
      {"kind", column_text(row, 3)},
      {"sourceTool", column_text(row, 4)},
      {"collectedAt", column_text(row, 5)},
      {"inputSummary", column_text(row, 6)},
      {"contentType", column_text(row, 7)},
      {"contentHash", column_text(row, 9)},
      {"truncated", sqlite3_column_int(row, 10) != 0},
      {"redactionStatus", column_text(row, 11)},
  };
  // Content is persisted as canonical JSON for both text and json payloads
  // (matching the shared `Evidence` contract), so a text body round-trips as the
  // underlying string rather than a quoted column value.
  evidence["content"] = parse_json_text(column_text(row, 8), "evidence content");
  add_optional_text(evidence, "runId", row, 12);
  return evidence;
}

nlohmann::json decode_investigation_finding(sqlite3_stmt* row) {
  nlohmann::json finding = {
      {"id", column_text(row, 0)},
      {"taskId", column_text(row, 1)},
      {"title", column_text(row, 2)},
      {"kind", column_text(row, 3)},
      {"statement", column_text(row, 4)},
      {"evidenceIds", parse_json_text(column_text(row, 5), "finding evidence ids")},
      {"confidence", column_text(row, 6)},
      {"createdAt", column_text(row, 8)},
  };
  add_optional_text(finding, "nextVerification", row, 7);
  return finding;
}

nlohmann::json decode_investigation_brief(sqlite3_stmt* row) {
  nlohmann::json brief = {
      {"id", column_text(row, 0)},
      {"taskId", column_text(row, 1)},
      {"generatedAt", column_text(row, 2)},
      {"status", column_text(row, 3)},
      {"findingIds", parse_json_text(column_text(row, 4), "brief finding ids")},
      {"options", parse_json_text(column_text(row, 5), "brief options")},
  };
  add_optional_text(brief, "selectedOptionId", row, 6);
  add_optional_text(brief, "planId", row, 7);
  return brief;
}

nlohmann::json decode_investigation_plan(sqlite3_stmt* row) {
  nlohmann::json plan = {
      {"id", column_text(row, 0)},
      {"taskId", column_text(row, 1)},
      {"revision", sqlite3_column_int64(row, 2)},
      {"status", column_text(row, 3)},
      {"createdAt", column_text(row, 4)},
      {"updatedAt", column_text(row, 5)},
      {"steps", parse_json_text(column_text(row, 7), "plan steps")},
  };
  add_optional_text(plan, "currentStepId", row, 6);
  if (sqlite3_column_type(row, 8) != SQLITE_NULL) {
    plan["approval"] = parse_json_text(column_text(row, 8), "plan approval");
  }
  if (sqlite3_column_type(row, 9) != SQLITE_NULL) {
    plan["observationWindow"] =
        parse_json_text(column_text(row, 9), "plan observation window");
  }
  return plan;
}

nlohmann::json decode_investigation_artifact(sqlite3_stmt* row) {
  nlohmann::json artifact = {
      {"id", column_text(row, 0)},
      {"taskId", column_text(row, 1)},
      {"phase", column_text(row, 3)},
      {"kind", column_text(row, 4)},
      {"status", column_text(row, 5)},
      {"title", column_text(row, 6)},
      {"summary", column_text(row, 7)},
      {"content", parse_json_text(column_text(row, 8), "artifact content")},
      {"evidenceIds", parse_json_text(column_text(row, 9), "artifact evidence ids")},
      {"createdAt", column_text(row, 10)},
      {"updatedAt", column_text(row, 11)},
  };
  add_optional_text(artifact, "runId", row, 2);
  add_optional_text(artifact, "planId", row, 12);
  add_optional_text(artifact, "planStepId", row, 13);
  return artifact;
}

nlohmann::json decode_investigation_run(sqlite3_stmt* row) {
  nlohmann::json run = {
      {"id", column_text(row, 0)},
      {"taskId", column_text(row, 1)},
      {"attempt", sqlite3_column_int64(row, 5)},
      {"phase", column_text(row, 6)},
      {"status", column_text(row, 7)},
      {"startedAt", column_text(row, 8)},
      {"updatedAt", column_text(row, 9)},
  };
  add_optional_text(run, "sessionId", row, 2);
  add_optional_text(run, "messageId", row, 3);
  add_optional_text(run, "traceId", row, 4);
  add_optional_text(run, "endedAt", row, 10);
  if (sqlite3_column_type(row, 11) != SQLITE_NULL) {
    run["checkpoint"] = parse_json_text(column_text(row, 11), "run checkpoint");
  }
  if (sqlite3_column_type(row, 12) != SQLITE_NULL) {
    run["failure"] = parse_json_text(column_text(row, 12), "run failure");
  }
  return run;
}

nlohmann::json decode_investigation_step(sqlite3_stmt* row) {
  nlohmann::json step = {
      {"id", column_text(row, 0)},
      {"taskId", column_text(row, 1)},
      {"runId", column_text(row, 2)},
      {"ordinal", sqlite3_column_int64(row, 3)},
      {"kind", column_text(row, 4)},
      {"title", column_text(row, 5)},
      {"status", column_text(row, 6)},
      {"attempt", sqlite3_column_int64(row, 7)},
      {"evidenceIds", parse_json_text(column_text(row, 12), "step evidence ids")},
  };
  add_optional_text(step, "toolName", row, 8);
  if (sqlite3_column_type(row, 9) != SQLITE_NULL) {
    step["target"] = parse_json_text(column_text(row, 9), "step target");
  }
  add_optional_text(step, "inputSummary", row, 10);
  add_optional_text(step, "outputSummary", row, 11);
  add_optional_text(step, "startedAt", row, 13);
  add_optional_text(step, "endedAt", row, 14);
  if (sqlite3_column_type(row, 15) != SQLITE_NULL) {
    step["failure"] = parse_json_text(column_text(row, 15), "step failure");
  }
  add_optional_text(step, "planId", row, 16);
  add_optional_text(step, "planStepId", row, 17);
  return step;
}

std::optional<nlohmann::json> load_investigation_task_row(sqlite3* connection,
                                                           const std::string& task_id) {
  Statement rows(connection, R"SQL(
      SELECT id, workspace_id, server_id, objective, success_criteria, scope, mode,
             permission_mode, automation_level, status, max_steps, max_run_ms,
             max_attempts, created_by, phase, created_at, updated_at, completed_at,
             active_run_id, last_failure, guardrails
        FROM investigation_tasks
       WHERE id = ?1
    )SQL");
  bind_text(connection, rows.get(), 1, task_id);
  const auto result = sqlite3_step(rows.get());
  if (result == SQLITE_DONE) return std::nullopt;
  if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection));
  return decode_investigation_task(rows.get());
}

// Decodes one `mcp_servers` row into the shared `McpServerConfig` shape.
nlohmann::json decode_mcp_server(sqlite3_stmt* row) {
  const auto transport = std::string(column_text(row, 2));
  if (!is_one_of(transport, {"stdio", "http"})) {
    throw std::runtime_error("Stored MCP server has an unknown transport");
  }
  const auto trust_level = std::string(column_text(row, 8));
  if (!is_one_of(trust_level, {"reviewed", "unreviewed"})) {
    throw std::runtime_error("Stored MCP server has an unknown trust level");
  }
  const auto annotation_trust = std::string(column_text(row, 11));
  if (!is_one_of(annotation_trust, {"none", "trusted"})) {
    throw std::runtime_error("Stored MCP server has an unknown annotation trust");
  }
  nlohmann::json config = {
      {"id", column_text(row, 0)},
      {"label", column_text(row, 1)},
      {"transport", transport},
      {"enabled", sqlite3_column_int(row, 6) != 0},
      {"allowedTools", mcp_json_array(row, 7, "MCP allowed tools")},
      {"trustLevel", trust_level},
      {"httpAuthHeaders", mcp_json_array(row, 9, "MCP HTTP auth headers")},
      {"annotationTrust", annotation_trust},
  };
  add_optional_text(config, "command", row, 3);
  add_optional_text(config, "url", row, 5);
  if (sqlite3_column_type(row, 4) != SQLITE_NULL) {
    auto args = parse_json_text(column_text(row, 4), "MCP server arguments");
    if (!args.is_array()) throw std::runtime_error("Stored MCP server arguments are invalid");
    config["args"] = std::move(args);
  }
  if (sqlite3_column_type(row, 10) != SQLITE_NULL) {
    auto oauth = parse_json_text(column_text(row, 10), "MCP OAuth config");
    if (!oauth.is_object()) throw std::runtime_error("Stored MCP OAuth config is invalid");
    config["oauth"] = std::move(oauth);
  }
  return config;
}

void execute_sql(sqlite3* connection, const char* sql) {
  char* error = nullptr;
  if (sqlite3_exec(connection, sql, nullptr, nullptr, &error) == SQLITE_OK) return;
  const std::string message = error == nullptr ? sqlite3_errmsg(connection) : error;
  sqlite3_free(error);
  throw std::runtime_error(message);
}

class Transaction {
 public:
  explicit Transaction(sqlite3* connection) : connection_(connection) {
    execute_sql(connection_, "BEGIN IMMEDIATE;");
  }
  ~Transaction() {
    if (active_) sqlite3_exec(connection_, "ROLLBACK;", nullptr, nullptr, nullptr);
  }
  Transaction(const Transaction&) = delete;
  Transaction& operator=(const Transaction&) = delete;

  void commit() {
    execute_sql(connection_, "COMMIT;");
    active_ = false;
  }

 private:
  sqlite3* connection_;
  bool active_ = true;
};

std::filesystem::path with_suffix(const std::filesystem::path& path,
                                  const char* suffix) {
  auto result = path;
  result += suffix;
  return result;
}

void remove_file_if_present(const std::filesystem::path& path) {
  std::error_code error;
  if (std::filesystem::remove(path, error) || !error) return;
  throw std::runtime_error("Could not remove " + path_as_utf8(path) + ": " + error.message());
}

std::string sha256_file(const std::filesystem::path& path) {
  std::ifstream input(path, std::ios::binary);
  if (!input) throw std::runtime_error("Could not read " + path_as_utf8(path));
  std::array<unsigned char, 64 * 1024> buffer{};
#ifdef _WIN32
  BCRYPT_ALG_HANDLE algorithm = nullptr;
  BCRYPT_HASH_HANDLE hash = nullptr;
  DWORD object_length = 0;
  DWORD bytes_written = 0;
  std::vector<unsigned char> object;
  std::array<unsigned char, 32> digest{};
  auto status = BCryptOpenAlgorithmProvider(&algorithm, BCRYPT_SHA256_ALGORITHM,
                                             nullptr, 0);
  if (status < 0) throw std::runtime_error("Could not initialize SHA-256");
  status = BCryptGetProperty(algorithm, BCRYPT_OBJECT_LENGTH,
                             reinterpret_cast<PUCHAR>(&object_length),
                             sizeof(object_length), &bytes_written, 0);
  if (status >= 0) {
    object.resize(object_length);
    status = BCryptCreateHash(algorithm, &hash, object.data(), object_length,
                              nullptr, 0, 0);
  }
  if (status < 0) {
    if (algorithm != nullptr) BCryptCloseAlgorithmProvider(algorithm, 0);
    throw std::runtime_error("Could not initialize SHA-256");
  }
  while (input) {
    input.read(reinterpret_cast<char*>(buffer.data()), buffer.size());
    const auto count = static_cast<ULONG>(input.gcount());
    if (count != 0 && BCryptHashData(hash, buffer.data(), count, 0) < 0) {
      BCryptDestroyHash(hash);
      BCryptCloseAlgorithmProvider(algorithm, 0);
      throw std::runtime_error("Could not compute the backup checksum");
    }
  }
  if (input.bad()) {
    BCryptDestroyHash(hash);
    BCryptCloseAlgorithmProvider(algorithm, 0);
    throw std::runtime_error("Could not read " + path_as_utf8(path));
  }
  status = BCryptFinishHash(hash, digest.data(), digest.size(), 0);
  BCryptDestroyHash(hash);
  BCryptCloseAlgorithmProvider(algorithm, 0);
  if (status < 0) throw std::runtime_error("Could not compute the backup checksum");
#elif defined(__APPLE__)
  CC_SHA256_CTX context;
  CC_SHA256_Init(&context);
  while (input) {
    input.read(reinterpret_cast<char*>(buffer.data()), buffer.size());
    const auto count = static_cast<std::size_t>(input.gcount());
    if (count != 0 && CC_SHA256_Update(&context, buffer.data(),
                                       static_cast<CC_LONG>(count)) != 1) {
      throw std::runtime_error("Could not compute the backup checksum");
    }
  }
  if (input.bad()) throw std::runtime_error("Could not read " + path_as_utf8(path));
  std::array<unsigned char, CC_SHA256_DIGEST_LENGTH> digest{};
  if (CC_SHA256_Final(digest.data(), &context) != 1) {
    throw std::runtime_error("Could not compute the backup checksum");
  }
#else
  auto* checksum = g_checksum_new(G_CHECKSUM_SHA256);
  if (checksum == nullptr) throw std::runtime_error("Could not initialize SHA-256");
  while (input) {
    input.read(reinterpret_cast<char*>(buffer.data()), buffer.size());
    const auto count = static_cast<std::size_t>(input.gcount());
    if (count != 0) g_checksum_update(checksum, buffer.data(), count);
  }
  if (input.bad()) {
    g_checksum_free(checksum);
    throw std::runtime_error("Could not read " + path_as_utf8(path));
  }
  const auto* value = g_checksum_get_string(checksum);
  const std::string digest = value == nullptr ? std::string() : value;
  g_checksum_free(checksum);
  if (digest.size() != 64) throw std::runtime_error("Could not compute the backup checksum");
  return digest;
#endif
  std::ostringstream rendered;
  rendered << std::hex << std::setfill('0');
  for (const auto byte : digest) rendered << std::setw(2) << static_cast<unsigned>(byte);
  return rendered.str();
}

std::string utc_timestamp() {
  const auto now = std::chrono::system_clock::now();
  const auto time = std::chrono::system_clock::to_time_t(now);
  std::tm utc{};
#ifdef _WIN32
  gmtime_s(&utc, &time);
#else
  gmtime_r(&time, &utc);
#endif
  std::ostringstream rendered;
  rendered << std::put_time(&utc, "%Y-%m-%dT%H:%M:%SZ");
  return rendered.str();
}

std::int64_t database_user_version(sqlite3* connection) {
  Statement statement(connection, "PRAGMA user_version;");
  if (sqlite3_step(statement.get()) != SQLITE_ROW) {
    throw std::runtime_error(sqlite3_errmsg(connection));
  }
  return sqlite3_column_int64(statement.get(), 0);
}

void create_pre_migration_backup(sqlite3* connection,
                                 const std::filesystem::path& database,
                                 std::int64_t source_version) {
  const auto backup = with_suffix(database, ".pre-migration.bak");
  const auto manifest = with_suffix(database, ".pre-migration.json");
  remove_file_if_present(backup);
  remove_file_if_present(manifest);

  Statement vacuum(connection, "VACUUM INTO ?1;");
  bind_text(connection, vacuum.get(), 1, path_as_utf8(backup));
  const auto result = sqlite3_step(vacuum.get());
  if (result != SQLITE_DONE) {
    throw std::runtime_error(std::string("Could not create migration backup: ") +
                             sqlite3_errmsg(connection));
  }
  std::error_code error;
  const auto bytes = std::filesystem::file_size(backup, error);
  if (error) throw std::runtime_error("Could not inspect migration backup: " + error.message());
  const nlohmann::json record = {
      {"sourceVersion", source_version},
      {"targetVersion", detail::kSchemaMigrations.size()},
      {"sha256", sha256_file(backup)},
      {"bytes", bytes},
      {"createdAt", utc_timestamp()},
  };
  std::ofstream output(manifest, std::ios::binary | std::ios::trunc);
  if (!output || !(output << record.dump(2) << '\n')) {
    throw std::runtime_error("Could not write migration manifest " + path_as_utf8(manifest));
  }
}

void apply_schema_migrations(sqlite3* connection) {
  const auto current = database_user_version(connection);
  const auto supported = static_cast<std::int64_t>(detail::kSchemaMigrations.size());
  if (current > supported) {
    throw std::runtime_error("Database schema is version " + std::to_string(current) +
                             ", this build supports " + std::to_string(supported));
  }
  for (std::int64_t version = std::max<std::int64_t>(1, current + 1);
       version <= supported; ++version) {
    try {
      Transaction transaction(connection);
      char* error = nullptr;
      const auto& sql = detail::kSchemaMigrations[static_cast<std::size_t>(version - 1)];
      const auto result = sqlite3_exec(connection, sql.data(), nullptr, nullptr, &error);
      if (result != SQLITE_OK) {
        const std::string message = error == nullptr ? sqlite3_errmsg(connection) : error;
        sqlite3_free(error);
        throw std::runtime_error(message);
      }
      execute_sql(connection, ("PRAGMA user_version=" + std::to_string(version) + ";").c_str());
      transaction.commit();
    } catch (const std::exception& error) {
      throw std::runtime_error("Migration to schema version " + std::to_string(version) +
                               " failed: " + error.what());
    }
  }
}

constexpr const char* kChatSessionSelect = R"SQL(
  SELECT s.id, s.workspace_id, s.server_id, s.title, s.created_at, s.updated_at,
         s.archived_at, COUNT(m.id),
         (SELECT substr(last_message.content, 1, 240)
            FROM chat_messages last_message
           WHERE last_message.session_id = s.id
           ORDER BY last_message.created_at DESC, last_message.id DESC
           LIMIT 1)
    FROM chat_sessions s
    LEFT JOIN chat_messages m ON m.session_id = s.id
)SQL";

std::string normalized_preview(std::string value) {
  std::string preview;
  preview.reserve(value.size());
  bool pending_space = false;
  for (const auto character : value) {
    if (std::isspace(static_cast<unsigned char>(character))) {
      pending_space = !preview.empty();
      continue;
    }
    if (pending_space) preview.push_back(' ');
    pending_space = false;
    preview.push_back(character);
  }
  return preview;
}

nlohmann::json chat_session_from_row(sqlite3* connection, sqlite3_stmt* row) {
  const auto message_count = sqlite3_column_int64(row, 7);
  if (message_count < 0 ||
      static_cast<std::uint64_t>(message_count) > std::numeric_limits<std::uint32_t>::max()) {
    throw std::runtime_error("Stored chat message count is out of range");
  }
  nlohmann::json session = {
      {"id", column_text(row, 0)},
      {"title", column_text(row, 3)},
      {"createdAt", column_text(row, 4)},
      {"updatedAt", column_text(row, 5)},
      {"messageCount", static_cast<std::uint32_t>(message_count)},
  };
  add_optional_text(session, "workspaceId", row, 1);
  add_optional_text(session, "serverId", row, 2);
  add_optional_text(session, "archivedAt", row, 6);
  if (sqlite3_column_type(row, 8) != SQLITE_NULL) {
    session["lastMessagePreview"] = normalized_preview(column_text(row, 8));
  }
  (void)connection;
  return session;
}

nlohmann::json chat_message_from_row(sqlite3* connection, sqlite3_stmt* row) {
  const std::string role = column_text(row, 2);
  if (!is_one_of(role, {"user", "assistant", "tool", "system"})) {
    throw std::runtime_error("Stored chat message has an unknown role");
  }
  nlohmann::json message = {
      {"id", column_text(row, 0)},
      {"sessionId", column_text(row, 1)},
      {"role", role},
      {"content", column_text(row, 3)},
      {"createdAt", column_text(row, 6)},
  };
  if (sqlite3_column_type(row, 4) != SQLITE_NULL) {
    try {
      auto parts = nlohmann::json::parse(column_text(row, 4));
      if (!parts.is_array()) throw std::runtime_error("Stored chat prompt parts are not an array");
      message["parts"] = std::move(parts);
    } catch (const nlohmann::json::exception&) {
      throw std::runtime_error("Stored chat prompt parts are invalid JSON");
    }
  }
  add_optional_text(message, "traceId", row, 5);
  (void)connection;
  return message;
}

nlohmann::json query_chat_session(sqlite3* connection, const std::string& session_id) {
  const std::string session_sql = std::string(kChatSessionSelect) +
      " WHERE s.id = ?1 GROUP BY s.id";
  Statement session_query(connection, session_sql.c_str());
  bind_text(connection, session_query.get(), 1, session_id);
  const auto session_result = sqlite3_step(session_query.get());
  if (session_result == SQLITE_DONE) throw std::runtime_error("Chat session was not found");
  if (session_result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection));
  auto session = chat_session_from_row(connection, session_query.get());

  Statement message_query(connection, R"SQL(
    SELECT id, session_id, role, content, parts_json, trace_id, created_at
      FROM chat_messages
     WHERE session_id = ?1
     ORDER BY created_at ASC, id ASC
  )SQL");
  bind_text(connection, message_query.get(), 1, session_id);
  nlohmann::json messages = nlohmann::json::array();
  for (;;) {
    const auto result = sqlite3_step(message_query.get());
    if (result == SQLITE_DONE) break;
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection));
    messages.push_back(chat_message_from_row(connection, message_query.get()));
  }
  return {{"session", std::move(session)}, {"messages", std::move(messages)}};
}

std::string escape_like(const std::string& value) {
  std::string escaped;
  escaped.reserve(value.size());
  for (const char character : value) {
    if (character == '\\' || character == '%' || character == '_') escaped.push_back('\\');
    escaped.push_back(character);
  }
  return escaped;
}

using SqlValue = std::variant<std::monostate, std::int64_t, std::string>;
using SqlField = std::pair<std::string, SqlValue>;

std::set<std::string> table_columns(sqlite3* connection, const char* table) {
  const auto sql = std::string("PRAGMA table_info(") + table + ")";
  Statement metadata(connection, sql.c_str());
  std::set<std::string> columns;
  for (;;) {
    const auto result = sqlite3_step(metadata.get());
    if (result == SQLITE_DONE) break;
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection));
    columns.emplace(column_text(metadata.get(), 1));
  }
  return columns;
}

std::optional<std::string> optional_json_text(const nlohmann::json& object,
                                              const char* key) {
  if (!object.contains(key) || object.at(key).is_null()) return std::nullopt;
  return object.at(key).dump();
}

std::optional<std::string> optional_string(const nlohmann::json& object,
                                           const char* key) {
  if (!object.contains(key) || object.at(key).is_null()) return std::nullopt;
  return object.at(key).get<std::string>();
}

void add_required_field(std::vector<SqlField>& fields, const char* name,
                        SqlValue value) {
  fields.emplace_back(name, std::move(value));
}

void add_optional_field(std::vector<SqlField>& fields,
                        const std::set<std::string>& columns, const char* name,
                        std::optional<std::string> value) {
  if (!columns.contains(name)) {
    if (value.has_value()) {
      throw std::runtime_error(std::string("The local database does not support ") + name);
    }
    return;
  }
  if (value.has_value()) fields.emplace_back(name, std::move(*value));
  else fields.emplace_back(name, std::monostate{});
}

std::vector<SqlField> server_fields(const nlohmann::json& server,
                                   const std::set<std::string>& columns,
                                   bool include_id) {
  const auto& connection = server.at("connection");
  const auto& metadata = server.at("metadata");
  std::vector<SqlField> fields;
  if (include_id) add_required_field(fields, "id", server.at("id").get<std::string>());
  add_required_field(fields, "name", server.at("name").get<std::string>());
  add_required_field(fields, "host", connection.at("host").get<std::string>());
  add_required_field(fields, "port", static_cast<std::int64_t>(connection.at("port").get<int>()));
  add_required_field(fields, "username", connection.at("username").get<std::string>());
  add_required_field(fields, "identity_id",
                     optional_string(connection, "identityId").has_value()
                         ? SqlValue(*optional_string(connection, "identityId"))
                         : SqlValue(std::monostate{}));
  add_required_field(fields, "group_id",
                     optional_string(server, "groupId").has_value()
                         ? SqlValue(*optional_string(server, "groupId"))
                         : SqlValue(std::monostate{}));
  add_required_field(fields, "capabilities", server.at("capabilities").dump());
  add_required_field(fields, "status", server.at("status").get<std::string>());
  add_required_field(fields, "environment", metadata.at("environment").get<std::string>());
  add_optional_field(fields, columns, "region", optional_string(metadata, "region"));
  add_optional_field(fields, columns, "hostname", optional_string(metadata, "hostname"));
  add_optional_field(fields, columns, "os", optional_string(metadata, "os"));
  add_optional_field(fields, columns, "tags", optional_json_text(metadata, "tags"));
  add_optional_field(fields, columns, "workspace_ids",
                     optional_json_text(metadata, "workspaceIds"));

  const auto authority = connection.find("hostCertificateAuthority");
  const bool has_authority = authority != connection.end() && !authority->is_null();
  std::optional<std::string> ca_public_key;
  std::optional<std::string> principals;
  std::optional<std::string> krl_path;
  std::optional<std::string> krl_url;
  std::optional<std::string> krl_signers;
  if (has_authority) {
    ca_public_key = authority->at("caPublicKey").get<std::string>();
    principals = authority->at("principals").dump();
    krl_path = optional_string(*authority, "revocationListPath");
    krl_url = optional_string(*authority, "revocationListUrl");
    if (authority->contains("revocationListSigners") &&
        !authority->at("revocationListSigners").empty()) {
      krl_signers = authority->at("revocationListSigners").dump();
    }
  }
  add_optional_field(fields, columns, "host_ca_public_key", std::move(ca_public_key));
  add_optional_field(fields, columns, "host_principals", std::move(principals));
  add_optional_field(fields, columns, "host_krl_path", std::move(krl_path));
  add_optional_field(fields, columns, "host_krl_url", std::move(krl_url));
  add_optional_field(fields, columns, "host_krl_signers", std::move(krl_signers));

  add_required_field(fields, "created_at", server.at("createdAt").get<std::string>());
  add_required_field(fields, "updated_at", server.at("updatedAt").get<std::string>());
  return fields;
}

void bind_sql_value(sqlite3* connection, sqlite3_stmt* statement, int index,
                    const SqlValue& value) {
  if (std::holds_alternative<std::monostate>(value)) {
    if (sqlite3_bind_null(statement, index) != SQLITE_OK) {
      throw std::runtime_error(sqlite3_errmsg(connection));
    }
    return;
  }
  if (const auto* integer = std::get_if<std::int64_t>(&value)) {
    if (sqlite3_bind_int64(statement, index, *integer) != SQLITE_OK) {
      throw std::runtime_error(sqlite3_errmsg(connection));
    }
    return;
  }
  bind_text(connection, statement, index, std::get<std::string>(value));
}

void write_server_row(sqlite3* connection, const nlohmann::json& server,
                      bool insert) {
  const auto columns = table_columns(connection, "servers");
  auto fields = server_fields(server, columns, insert);
  std::string sql;
  if (insert) {
    sql = "INSERT INTO servers (";
    for (std::size_t index = 0; index < fields.size(); ++index) {
      if (index != 0) sql += ", ";
      sql += fields[index].first;
    }
    sql += ") VALUES (";
  } else {
    sql = "UPDATE servers SET ";
    for (std::size_t index = 0; index < fields.size(); ++index) {
      if (index != 0) sql += ", ";
      sql += fields[index].first + " = ?" + std::to_string(index + 1);
    }
    sql += " WHERE id = ?" + std::to_string(fields.size() + 1);
  }
  if (insert) {
    for (std::size_t index = 0; index < fields.size(); ++index) {
      if (index != 0) sql += ", ";
      sql += "?" + std::to_string(index + 1);
    }
    sql += ")";
  }
  Statement statement(connection, sql.c_str());
  for (std::size_t index = 0; index < fields.size(); ++index) {
    bind_sql_value(connection, statement.get(), static_cast<int>(index + 1),
                   fields[index].second);
  }
  if (!insert) {
    bind_text(connection, statement.get(), static_cast<int>(fields.size() + 1),
              server.at("id").get<std::string>());
  }
  if (sqlite3_step(statement.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection));
  }
  if (!insert && sqlite3_changes(connection) == 0) {
    throw std::runtime_error("Server was not found");
  }
}

void insert_identity_row(sqlite3* connection, const nlohmann::json& identity) {
  const auto columns = table_columns(connection, "identities");
  std::vector<SqlField> fields;
  add_required_field(fields, "id", identity.at("id").get<std::string>());
  add_required_field(fields, "label", identity.at("label").get<std::string>());
  add_required_field(fields, "method", identity.at("method").get<std::string>());
  add_required_field(fields, "credential_ref", identity.at("credentialRef").get<std::string>());
  add_optional_field(fields, columns, "passphrase_ref",
                     optional_string(identity, "passphraseRef"));
  add_optional_field(fields, columns, "private_key_path",
                     optional_string(identity, "privateKeyPath"));
  add_optional_field(fields, columns, "certificate_path",
                     optional_string(identity, "certificatePath"));
  add_required_field(fields, "created_at", identity.at("createdAt").get<std::string>());

  std::string sql = "INSERT INTO identities (";
  for (std::size_t index = 0; index < fields.size(); ++index) {
    if (index != 0) sql += ", ";
    sql += fields[index].first;
  }
  sql += ") VALUES (";
  for (std::size_t index = 0; index < fields.size(); ++index) {
    if (index != 0) sql += ", ";
    sql += "?" + std::to_string(index + 1);
  }
  sql += ")";
  Statement statement(connection, sql.c_str());
  for (std::size_t index = 0; index < fields.size(); ++index) {
    bind_sql_value(connection, statement.get(), static_cast<int>(index + 1),
                   fields[index].second);
  }
  if (sqlite3_step(statement.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection));
  }
}

void attach_identity(sqlite3* connection, const std::string& server_id,
                     const nlohmann::json& server) {
  const auto identity_id = server.at("connection").value("identityId", std::string());
  if (identity_id.empty()) return;
  Statement attach(connection, R"SQL(
    INSERT OR IGNORE INTO server_identities (server_id, identity_id)
    VALUES (?1, ?2)
  )SQL");
  bind_text(connection, attach.get(), 1, server_id);
  bind_text(connection, attach.get(), 2, identity_id);
  if (sqlite3_step(attach.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection));
  }
}

void record_server_activity(sqlite3* connection, const std::string& id,
                            const std::string& server_id, const char* title,
                            const std::string& timestamp) {
  Statement activity(connection, R"SQL(
    INSERT INTO activities (
      id, server_id, workspace_id, type, title, description, source, actor,
      reason, outcome, trace_id, created_at
    ) VALUES (?1, ?2, NULL, 'configuration', ?3, NULL, 'user', 'user',
              '用户在工作区执行操作', 'success', NULL, ?4)
  )SQL");
  bind_text(connection, activity.get(), 1, id);
  bind_text(connection, activity.get(), 2, server_id);
  bind_text(connection, activity.get(), 3, title);
  bind_text(connection, activity.get(), 4, timestamp);
  if (sqlite3_step(activity.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection));
  }
}

std::optional<std::string> read_server_identity(sqlite3* connection,
                                               const std::string& server_id) {
  Statement statement(connection, "SELECT identity_id FROM servers WHERE id = ?1");
  bind_text(connection, statement.get(), 1, server_id);
  const auto result = sqlite3_step(statement.get());
  if (result == SQLITE_DONE) return std::nullopt;
  if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection));
  if (sqlite3_column_type(statement.get(), 0) == SQLITE_NULL) return std::nullopt;
  return column_text(statement.get(), 0);
}

std::optional<nlohmann::json> read_identity(sqlite3* connection,
                                            const std::string& identity_id) {
  const auto columns = table_columns(connection, "identities");
  const auto select = [&columns](const char* column) {
    return columns.contains(column) ? std::string(column)
                                    : std::string("NULL AS ") + column;
  };
  const auto sql = std::string("SELECT id, label, method, credential_ref, ") +
                   select("passphrase_ref") + ", " + select("private_key_path") +
                   ", " + select("certificate_path") +
                   ", created_at FROM identities WHERE id = ?1";
  Statement statement(connection, sql.c_str());
  bind_text(connection, statement.get(), 1, identity_id);
  const auto result = sqlite3_step(statement.get());
  if (result == SQLITE_DONE) return std::nullopt;
  if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection));
  nlohmann::json identity = {
      {"id", column_text(statement.get(), 0)},
      {"label", column_text(statement.get(), 1)},
      {"method", column_text(statement.get(), 2)},
      {"credentialRef", column_text(statement.get(), 3)},
      {"createdAt", column_text(statement.get(), 7)},
  };
  add_optional_text(identity, "passphraseRef", statement.get(), 4);
  add_optional_text(identity, "privateKeyPath", statement.get(), 5);
  add_optional_text(identity, "certificatePath", statement.get(), 6);
  return identity;
}

// ── Investigation retention helpers ─────────────────────────────────────────
bool iso8601_utc_text(const std::string& value) {
  if (value.size() < 19 || value.size() > 80) return false;
  std::tm utc{};
  std::istringstream stream(value);
  stream >> std::get_time(&utc, "%Y-%m-%dT%H:%M:%S");
  if (stream.fail()) return false;
#ifdef _WIN32
  return _mkgmtime(&utc) != static_cast<std::time_t>(-1);
#else
  return timegm(&utc) != static_cast<std::time_t>(-1);
#endif
}

void collect_json_string_ids(const std::string& raw, std::set<std::string>& out) {
  nlohmann::json parsed;
  try {
    parsed = nlohmann::json::parse(raw);
  } catch (const nlohmann::json::exception&) {
    return;
  }
  if (!parsed.is_array()) return;
  for (const auto& entry : parsed) {
    if (entry.is_string()) out.insert(entry.get<std::string>());
  }
}

// Evidence ids referenced by any finding, step, artifact, brief option or plan
// step are never retention candidates.
std::set<std::string> collect_protected_evidence(sqlite3* connection,
                                                 const std::string& task_id) {
  std::set<std::string> protected_ids;
  const auto collect_direct = [&](const char* sql) {
    Statement statement(connection, sql);
    bind_text(connection, statement.get(), 1, task_id);
    for (;;) {
      const auto result = sqlite3_step(statement.get());
      if (result == SQLITE_DONE) break;
      if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection));
      collect_json_string_ids(column_text(statement.get(), 0), protected_ids);
    }
  };
  collect_direct("SELECT evidence_ids FROM investigation_findings WHERE task_id = ?1");
  collect_direct("SELECT evidence_ids FROM investigation_steps WHERE task_id = ?1");
  collect_direct("SELECT evidence_ids FROM investigation_artifacts WHERE task_id = ?1");
  const auto collect_nested = [&](const char* sql) {
    Statement statement(connection, sql);
    bind_text(connection, statement.get(), 1, task_id);
    for (;;) {
      const auto result = sqlite3_step(statement.get());
      if (result == SQLITE_DONE) break;
      if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection));
      nlohmann::json parsed;
      try {
        parsed = nlohmann::json::parse(column_text(statement.get(), 0));
      } catch (const nlohmann::json::exception&) {
        continue;
      }
      if (!parsed.is_array()) continue;
      for (const auto& entry : parsed) {
        if (!entry.is_object()) continue;
        const auto ids = entry.find("evidenceIds");
        if (ids == entry.end() || !ids->is_array()) continue;
        for (const auto& id : *ids) {
          if (id.is_string()) protected_ids.insert(id.get<std::string>());
        }
      }
    }
  };
  collect_nested("SELECT options FROM investigation_decision_briefs WHERE task_id = ?1");
  collect_nested("SELECT steps FROM investigation_plans WHERE task_id = ?1");
  return protected_ids;
}

std::optional<std::string> investigation_task_status(sqlite3* connection,
                                                     const std::string& task_id) {
  Statement statement(connection, "SELECT status FROM investigation_tasks WHERE id = ?1");
  bind_text(connection, statement.get(), 1, task_id);
  const auto result = sqlite3_step(statement.get());
  if (result == SQLITE_DONE) return std::nullopt;
  if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection));
  return std::string(column_text(statement.get(), 0));
}

bool terminal_task_status(const std::string& status) {
  return is_one_of(status, {"completed", "failed", "stopped", "expired"});
}

}  // namespace

DesktopDatabase::DesktopDatabase(const std::filesystem::path& data_directory) {
  if (!data_directory.is_absolute()) {
    throw std::runtime_error("The Yukinal data directory must be an absolute path");
  }
  data_directory_ = data_directory;
  std::error_code error;
  std::filesystem::create_directories(data_directory, error);
  if (error) {
    throw std::runtime_error("Could not create the Yukinal data directory: " + error.message());
  }
  const auto file_path = data_directory / "yukinal.db";
  const bool database_exists = std::filesystem::exists(file_path, error);
  if (error) throw std::runtime_error("Could not inspect the Yukinal database path");
  const bool existing_nonempty = database_exists &&
      std::filesystem::file_size(file_path, error) > 0;
  if (error) throw std::runtime_error("Could not inspect the Yukinal database file");

  const auto database_path = path_as_utf8(file_path);
  const auto result = sqlite3_open_v2(database_path.c_str(), &connection_,
                                      SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE |
                                          SQLITE_OPEN_FULLMUTEX,
                                      nullptr);
  if (result != SQLITE_OK) {
    const std::string message = connection_ == nullptr
                                    ? "Could not open the Yukinal database"
                                    : sqlite3_errmsg(connection_);
    if (connection_ != nullptr) sqlite3_close_v2(connection_);
    connection_ = nullptr;
    throw std::runtime_error(message);
  }
  sqlite3_busy_timeout(connection_, 5000);
  bool backup_ready = false;
  try {
    execute_sql(connection_, "PRAGMA foreign_keys=ON;");
    execute_sql(connection_, "PRAGMA journal_mode=WAL;");
    execute_sql(connection_, "PRAGMA trusted_schema=OFF;");
    const auto current = database_user_version(connection_);
    const auto supported = static_cast<std::int64_t>(detail::kSchemaMigrations.size());
    if (existing_nonempty && current < supported) {
      create_pre_migration_backup(connection_, file_path, current);
      backup_ready = true;
    }
    apply_schema_migrations(connection_);
    sqlite3_db_config(connection_, SQLITE_DBCONFIG_DEFENSIVE, 1, nullptr);
  } catch (const std::exception& migration_error) {
    const std::string message = migration_error.what();
    sqlite3_close_v2(connection_);
    connection_ = nullptr;
    if (backup_ready) {
      const auto backup = with_suffix(file_path, ".pre-migration.bak");
      try {
        std::filesystem::copy_file(backup, file_path,
                                   std::filesystem::copy_options::overwrite_existing);
        remove_file_if_present(with_suffix(file_path, "-wal"));
        remove_file_if_present(with_suffix(file_path, "-shm"));
      } catch (const std::exception& rollback_error) {
        throw std::runtime_error("Migration failed and rollback from " +
                                 path_as_utf8(backup) + " also failed: " +
                                 rollback_error.what() + " (migration: " + message + ")");
      }
      throw std::runtime_error("Migration failed and the database was restored from " +
                               path_as_utf8(backup) + ": " + message);
    }
    throw;
  }
}

DesktopDatabase::~DesktopDatabase() {
  if (connection_ != nullptr) sqlite3_close_v2(connection_);
}

bool DesktopDatabase::is_available() const {
  std::lock_guard lock(mutex_);
  return connection_ != nullptr;
}

std::filesystem::path DesktopDatabase::data_directory() const {
  std::lock_guard lock(mutex_);
  return data_directory_;
}

std::optional<nlohmann::json> DesktopDatabase::get_server_endpoint(
    const std::string& server_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local database is unavailable");
  Statement statement(connection_, "SELECT host, port FROM servers WHERE id = ?1;");
  bind_text(connection_, statement.get(), 1, server_id);
  if (sqlite3_step(statement.get()) != SQLITE_ROW) return std::nullopt;
  return nlohmann::json{{"host", column_text(statement.get(), 0)},
                        {"port", sqlite3_column_int(statement.get(), 1)}};
}

std::optional<nlohmann::json> DesktopDatabase::get_server_connection(
    const std::string& server_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local database is unavailable");
  const auto columns = table_columns(connection_, "servers");
  const auto select_optional = [&columns](const char* name) {
    return columns.contains(name) ? std::string(name) : std::string("NULL AS ") + name;
  };
  const auto sql = std::string(
      "SELECT host, port, username, identity_id, ") +
      select_optional("host_ca_public_key") + ", " +
      select_optional("host_principals") + ", " +
      select_optional("host_krl_path") + ", " +
      select_optional("host_krl_url") + ", " +
      select_optional("host_krl_signers") +
      " FROM servers WHERE id = ?1";
  Statement statement(connection_, sql.c_str());
  bind_text(connection_, statement.get(), 1, server_id);
  const auto result = sqlite3_step(statement.get());
  if (result == SQLITE_DONE) return std::nullopt;
  if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
  nlohmann::json record = {{"host", column_text(statement.get(), 0)},
                           {"port", sqlite3_column_int(statement.get(), 1)},
                           {"username", column_text(statement.get(), 2)}};
  add_optional_text(record, "identityId", statement.get(), 3);
  add_optional_text(record, "hostCertificateAuthorityPublicKey", statement.get(), 4);
  add_optional_text(record, "hostPrincipals", statement.get(), 5);
  add_optional_text(record, "hostRevocationListPath", statement.get(), 6);
  add_optional_text(record, "hostRevocationListUrl", statement.get(), 7);
  add_optional_text(record, "hostRevocationListSigners", statement.get(), 8);
  return record;
}

void DesktopDatabase::set_server_status(const std::string& server_id,
                                        const std::string& status,
                                        const std::string& timestamp) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local database is unavailable");
  if (!is_one_of(status, {"connecting", "connected", "disconnected", "error"})) {
    throw std::runtime_error("The server status is invalid");
  }
  Statement statement(connection_,
                      "UPDATE servers SET status = ?2, updated_at = ?3 WHERE id = ?1");
  bind_text(connection_, statement.get(), 1, server_id);
  bind_text(connection_, statement.get(), 2, status);
  bind_text(connection_, statement.get(), 3, timestamp);
  if (sqlite3_step(statement.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  if (sqlite3_changes(connection_) == 0) throw std::runtime_error("Server was not found");
}

void DesktopDatabase::record_connection_activity(
    const std::string& id, const std::string& server_id, const std::string& title,
    const std::optional<std::string>& description, const std::string& outcome,
    const std::string& timestamp) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local database is unavailable");
  if (!is_one_of(outcome, {"success", "failure", "cancelled", "denied"})) {
    throw std::runtime_error("The connection activity outcome is invalid");
  }
  Statement statement(connection_, R"SQL(
      INSERT INTO activities (
        id, server_id, workspace_id, type, title, description, source, actor,
        reason, outcome, trace_id, created_at
      ) VALUES (?1, ?2, NULL, 'connection', ?3, ?4, 'user', 'user', NULL, ?5, NULL, ?6)
    )SQL");
  bind_text(connection_, statement.get(), 1, id);
  bind_text(connection_, statement.get(), 2, server_id);
  bind_text(connection_, statement.get(), 3, title);
  bind_optional_text(connection_, statement.get(), 4, description);
  bind_text(connection_, statement.get(), 5, outcome);
  bind_text(connection_, statement.get(), 6, timestamp);
  if (sqlite3_step(statement.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
}

nlohmann::json DesktopDatabase::list_servers() const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) return {{"servers", nlohmann::json::array()}};
  std::set<std::string> columns;
  {
    Statement metadata(connection_, "PRAGMA table_info(servers)");
    for (;;) {
      const auto result = sqlite3_step(metadata.get());
      if (result == SQLITE_DONE) break;
      if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
      columns.emplace(column_text(metadata.get(), 1));
    }
  }
  const auto select_optional = [&columns](const char* name) {
    return columns.contains(name) ? std::string(name) : std::string("NULL AS ") + name;
  };
  const auto sql = std::string(R"SQL(
    SELECT id, name, host, port, username, identity_id, group_id, capabilities,
           status, environment, region, hostname, os, tags, workspace_ids,
  )SQL") + select_optional("host_ca_public_key") + ", " +
                   select_optional("host_principals") + ", " +
                   select_optional("host_krl_path") + ", " +
                   select_optional("host_krl_url") + ", " +
                   select_optional("host_krl_signers") + R"SQL(,
           created_at, updated_at
      FROM servers
     ORDER BY name COLLATE NOCASE, id
  )SQL";
  Statement rows(connection_, sql.c_str());
  nlohmann::json servers = nlohmann::json::array();
  for (;;) {
    const auto result = sqlite3_step(rows.get());
    if (result == SQLITE_DONE) break;
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));

    const auto capabilities = parse_json_column(rows.get(), 7, "capabilities");
    if (!capabilities.is_object()) throw std::runtime_error("Stored server capabilities are not an object");

    nlohmann::json connection = {
        {"host", column_text(rows.get(), 2)},
        {"port", sqlite3_column_int(rows.get(), 3)},
        {"username", column_text(rows.get(), 4)},
    };
    if (sqlite3_column_type(rows.get(), 5) != SQLITE_NULL) {
      connection["identityId"] = column_text(rows.get(), 5);
    }

    if (sqlite3_column_type(rows.get(), 15) != SQLITE_NULL) {
      nlohmann::json authority = {
          {"caPublicKey", column_text(rows.get(), 15)},
          {"principals", parse_json_column(rows.get(), 16, "host principals")},
      };
      if (!authority["principals"].is_array()) {
        throw std::runtime_error("Stored host principals are not an array");
      }
      add_optional_text(authority, "revocationListPath", rows.get(), 17);
      add_optional_text(authority, "revocationListUrl", rows.get(), 18);
      if (const auto signers = parse_optional_json_column(rows.get(), 19, "host KRL signers")) {
        if (!signers->is_array()) throw std::runtime_error("Stored host KRL signers are not an array");
        if (!signers->empty()) authority["revocationListSigners"] = *signers;
      }
      connection["hostCertificateAuthority"] = std::move(authority);
    } else if (sqlite3_column_type(rows.get(), 16) != SQLITE_NULL ||
               sqlite3_column_type(rows.get(), 17) != SQLITE_NULL ||
               sqlite3_column_type(rows.get(), 18) != SQLITE_NULL ||
               sqlite3_column_type(rows.get(), 19) != SQLITE_NULL) {
      throw std::runtime_error("Stored host certificate authority is incomplete");
    }

    nlohmann::json server = {
        {"id", column_text(rows.get(), 0)},
        {"name", column_text(rows.get(), 1)},
        {"connection", std::move(connection)},
        {"capabilities", capabilities},
        {"status", column_text(rows.get(), 8)},
        {"metadata", {{"environment", column_text(rows.get(), 9)}}},
        {"createdAt", column_text(rows.get(), 20)},
        {"updatedAt", column_text(rows.get(), 21)},
    };
    if (sqlite3_column_type(rows.get(), 6) != SQLITE_NULL) {
      server["groupId"] = column_text(rows.get(), 6);
    }
    add_optional_text(server["metadata"], "region", rows.get(), 10);
    add_optional_text(server["metadata"], "hostname", rows.get(), 11);
    add_optional_text(server["metadata"], "os", rows.get(), 12);
    if (const auto tags = parse_optional_json_column(rows.get(), 13, "tags")) {
      if (!tags->is_array()) throw std::runtime_error("Stored server tags are not an array");
      server["metadata"]["tags"] = *tags;
    }
    if (const auto workspace_ids = parse_optional_json_column(rows.get(), 14, "workspace ids")) {
      if (!workspace_ids->is_array()) throw std::runtime_error("Stored workspace ids are not an array");
      server["metadata"]["workspaceIds"] = *workspace_ids;
    }
    servers.push_back(std::move(server));
  }
  return {{"servers", std::move(servers)}};
}

std::optional<nlohmann::json> DesktopDatabase::get_identity(
    const std::string& identity_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local identity database is unavailable");
  return read_identity(connection_, identity_id);
}

void DesktopDatabase::add_server(
    const nlohmann::json& server,
    const std::optional<nlohmann::json>& new_identity,
    const std::string& activity_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local server database is unavailable");
  Transaction transaction(connection_);
  if (new_identity.has_value()) insert_identity_row(connection_, *new_identity);
  write_server_row(connection_, server, true);
  const auto identity_id = server.at("connection").value("identityId", std::string());
  if (!identity_id.empty() && !read_identity(connection_, identity_id).has_value()) {
    throw std::runtime_error("The selected SSH identity was not found");
  }
  attach_identity(connection_, server.at("id").get<std::string>(), server);
  record_server_activity(connection_, activity_id,
                         server.at("id").get<std::string>(), "已添加服务器",
                         server.at("updatedAt").get<std::string>());
  transaction.commit();
}

std::optional<std::string> DesktopDatabase::update_server(
    const nlohmann::json& server,
    const std::optional<nlohmann::json>& new_identity,
    const std::string& activity_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local server database is unavailable");
  const auto server_id = server.at("id").get<std::string>();
  Transaction transaction(connection_);
  const auto old_identity_id = read_server_identity(connection_, server_id);
  if (!old_identity_id.has_value()) {
    Statement exists(connection_, "SELECT 1 FROM servers WHERE id = ?1");
    bind_text(connection_, exists.get(), 1, server_id);
    const auto result = sqlite3_step(exists.get());
    if (result == SQLITE_DONE) throw std::runtime_error("Server was not found");
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  if (new_identity.has_value()) insert_identity_row(connection_, *new_identity);
  write_server_row(connection_, server, false);
  const auto new_identity_id = server.at("connection").value("identityId", std::string());
  if (!new_identity_id.empty() && !read_identity(connection_, new_identity_id).has_value()) {
    throw std::runtime_error("The selected SSH identity was not found");
  }
  attach_identity(connection_, server_id, server);
  if (old_identity_id.has_value() && *old_identity_id != new_identity_id) {
    Statement detach(connection_, R"SQL(
      DELETE FROM server_identities WHERE server_id = ?1 AND identity_id = ?2
    )SQL");
    bind_text(connection_, detach.get(), 1, server_id);
    bind_text(connection_, detach.get(), 2, *old_identity_id);
    if (sqlite3_step(detach.get()) != SQLITE_DONE) {
      throw std::runtime_error(sqlite3_errmsg(connection_));
    }
  }
  record_server_activity(connection_, activity_id, server_id, "已更新服务器配置",
                         server.at("updatedAt").get<std::string>());
  transaction.commit();
  if (old_identity_id.has_value() && *old_identity_id != new_identity_id) {
    return old_identity_id;
  }
  return std::nullopt;
}

std::optional<std::string> DesktopDatabase::delete_server(
    const std::string& server_id, const std::string& activity_id,
    const std::string& timestamp) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local server database is unavailable");
  Transaction transaction(connection_);
  const auto identity_id = read_server_identity(connection_, server_id);
  Statement remove(connection_, "DELETE FROM servers WHERE id = ?1");
  bind_text(connection_, remove.get(), 1, server_id);
  if (sqlite3_step(remove.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  if (sqlite3_changes(connection_) == 0) throw std::runtime_error("Server was not found");
  record_server_activity(connection_, activity_id, server_id, "已删除服务器", timestamp);
  transaction.commit();
  return identity_id;
}

std::vector<std::string> DesktopDatabase::delete_identity_if_unshared(
    const std::string& identity_id, const std::string& except_server_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local identity database is unavailable");
  Transaction transaction(connection_);
  Statement shared(connection_, R"SQL(
    SELECT EXISTS(
      SELECT 1 FROM server_identities
       WHERE identity_id = ?1 AND server_id <> ?2
      UNION ALL
      SELECT 1 FROM servers WHERE identity_id = ?1 AND id <> ?2
    )
  )SQL");
  bind_text(connection_, shared.get(), 1, identity_id);
  bind_text(connection_, shared.get(), 2, except_server_id);
  const auto shared_result = sqlite3_step(shared.get());
  if (shared_result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
  if (sqlite3_column_int(shared.get(), 0) != 0) {
    transaction.commit();
    return {};
  }
  const auto identity = read_identity(connection_, identity_id);
  if (!identity.has_value()) {
    transaction.commit();
    return {};
  }
  Statement remove(connection_, "DELETE FROM identities WHERE id = ?1");
  bind_text(connection_, remove.get(), 1, identity_id);
  if (sqlite3_step(remove.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  transaction.commit();

  std::vector<std::string> references;
  const auto credential_ref = identity->at("credentialRef").get<std::string>();
  if (!credential_ref.empty()) references.push_back(credential_ref);
  if (identity->contains("passphraseRef")) {
    const auto passphrase_ref = identity->at("passphraseRef").get<std::string>();
    if (!passphrase_ref.empty()) references.push_back(passphrase_ref);
  }
  return references;
}

void DesktopDatabase::queue_credential_cleanup(const std::string& reference,
                                               const std::string& error,
                                               const std::string& timestamp) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local credential database is unavailable");
  Statement enqueue(connection_, R"SQL(
    INSERT INTO credential_cleanup_queue (reference, created_at, attempts, last_error)
    VALUES (?1, ?2, 1, ?3)
    ON CONFLICT(reference) DO UPDATE SET
      attempts = credential_cleanup_queue.attempts + 1,
      last_error = excluded.last_error
  )SQL");
  bind_text(connection_, enqueue.get(), 1, reference);
  bind_text(connection_, enqueue.get(), 2, timestamp);
  bind_text(connection_, enqueue.get(), 3, error);
  if (sqlite3_step(enqueue.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
}

std::vector<std::string> DesktopDatabase::credential_cleanup_references() const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local credential database is unavailable");
  Statement rows(connection_,
                 "SELECT reference FROM credential_cleanup_queue ORDER BY created_at, reference");
  std::vector<std::string> references;
  for (;;) {
    const auto result = sqlite3_step(rows.get());
    if (result == SQLITE_DONE) break;
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
    references.emplace_back(column_text(rows.get(), 0));
  }
  return references;
}

void DesktopDatabase::clear_credential_cleanup(const std::string& reference) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local credential database is unavailable");
  Statement remove(connection_, "DELETE FROM credential_cleanup_queue WHERE reference = ?1");
  bind_text(connection_, remove.get(), 1, reference);
  if (sqlite3_step(remove.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
}

nlohmann::json DesktopDatabase::list_workspaces() const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) return {{"workspaces", nlohmann::json::array()}};
  Statement rows(connection_, R"SQL(
    SELECT id, name, server_ids, repositories, provider_ids, default_environment
      FROM workspaces
     ORDER BY name COLLATE NOCASE, id
  )SQL");
  nlohmann::json workspaces = nlohmann::json::array();
  for (;;) {
    const auto result = sqlite3_step(rows.get());
    if (result == SQLITE_DONE) break;
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
    auto server_ids = parse_json_column(rows.get(), 2, "workspace server ids");
    auto repositories = parse_json_column(rows.get(), 3, "workspace repositories");
    auto provider_ids = parse_json_column(rows.get(), 4, "workspace provider ids");
    if (!server_ids.is_array() || !repositories.is_array() || !provider_ids.is_array()) {
      throw std::runtime_error("Stored workspace collections must be arrays");
    }
    workspaces.push_back({
        {"id", column_text(rows.get(), 0)},
        {"name", column_text(rows.get(), 1)},
        {"serverIds", std::move(server_ids)},
        {"repositories", std::move(repositories)},
        {"providerIds", std::move(provider_ids)},
        {"defaultEnvironment", column_text(rows.get(), 5)},
    });
  }
  return {{"workspaces", std::move(workspaces)}};
}

DesktopNetworkProxyConfig DesktopDatabase::network_proxy_config() const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local database is unavailable");
  Statement setting(connection_,
                    "SELECT value FROM app_settings WHERE key = 'network.proxy';");
  const auto result = sqlite3_step(setting.get());
  if (result == SQLITE_DONE) return {};
  if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));

  nlohmann::json stored;
  try {
    stored = nlohmann::json::parse(column_text(setting.get(), 0));
  } catch (const nlohmann::json::exception&) {
    throw std::runtime_error("Stored network proxy setting is invalid JSON");
  }
  if (!stored.is_object() || !stored.contains("mode") || !stored.at("mode").is_string()) {
    throw std::runtime_error("Stored network proxy setting is malformed");
  }
  DesktopNetworkProxyConfig config;
  config.mode = stored.at("mode").get<std::string>();
  if (config.mode != "direct" && config.mode != "system") {
    throw std::runtime_error("Stored network proxy mode is unknown");
  }
  if (stored.contains("credentialRef") && !stored.at("credentialRef").is_null()) {
    if (!stored.at("credentialRef").is_string() ||
        stored.at("credentialRef").get_ref<const std::string&>().empty()) {
      throw std::runtime_error("Stored network proxy credential reference is malformed");
    }
    config.credential_ref = stored.at("credentialRef").get<std::string>();
  }
  return config;
}

void DesktopDatabase::save_network_proxy_config(
    const DesktopNetworkProxyConfig& config) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local database is unavailable");
  if (config.mode != "direct" && config.mode != "system") {
    throw std::invalid_argument("Network proxy mode must be direct or system");
  }
  nlohmann::json stored = {{"mode", config.mode}};
  if (config.credential_ref) stored["credentialRef"] = *config.credential_ref;
  const auto value = stored.dump();
  Statement save(connection_, R"SQL(
    INSERT INTO app_settings (key, value, updated_at)
    VALUES ('network.proxy', ?1, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    ON CONFLICT(key) DO UPDATE SET value = excluded.value,
      updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
  )SQL");
  bind_text(connection_, save.get(), 1, value);
  if (sqlite3_step(save.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
}

nlohmann::json DesktopDatabase::list_activities(
    const std::optional<std::string>& server_id, int limit) const {
  std::lock_guard lock(mutex_);
  if (limit < 1 || limit > 100) {
    throw std::invalid_argument("Activity limit must be between 1 and 100");
  }
  if (connection_ == nullptr) return {{"activities", nlohmann::json::array()}};

  const char* sql = server_id.has_value()
      ? R"SQL(
          SELECT id, server_id, workspace_id, type, title, description, source, actor,
                 reason, outcome, trace_id, created_at
            FROM activities
           WHERE server_id = ?1
           ORDER BY created_at DESC
           LIMIT ?2
        )SQL"
      : R"SQL(
          SELECT id, server_id, workspace_id, type, title, description, source, actor,
                 reason, outcome, trace_id, created_at
            FROM activities
           ORDER BY created_at DESC
           LIMIT ?1
        )SQL";
  Statement rows(connection_, sql);
  int limit_parameter = 1;
  if (server_id.has_value()) {
    const auto bind_result = sqlite3_bind_text(rows.get(), 1, server_id->c_str(),
                                               static_cast<int>(server_id->size()),
                                               SQLITE_TRANSIENT);
    if (bind_result != SQLITE_OK) throw std::runtime_error(sqlite3_errmsg(connection_));
    limit_parameter = 2;
  }
  if (sqlite3_bind_int(rows.get(), limit_parameter, limit) != SQLITE_OK) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }

  nlohmann::json activities = nlohmann::json::array();
  for (;;) {
    const auto result = sqlite3_step(rows.get());
    if (result == SQLITE_DONE) break;
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));

    const std::string type = column_text(rows.get(), 3);
    const std::string source = column_text(rows.get(), 6);
    if (!is_one_of(type, {"connection", "authentication", "configuration", "deployment",
                          "service", "container", "file_change", "agent_action",
                          "approval", "health"}) ||
        !is_one_of(source, {"agent", "user", "system", "docker", "git", "cloud"})) {
      throw std::runtime_error("Stored activity has an unknown type or source");
    }

    nlohmann::json activity = {
        {"id", column_text(rows.get(), 0)},
        {"type", type},
        {"title", column_text(rows.get(), 4)},
        {"source", source},
        {"actor", column_text(rows.get(), 7)},
        {"createdAt", column_text(rows.get(), 11)},
    };
    add_optional_text(activity, "serverId", rows.get(), 1);
    add_optional_text(activity, "workspaceId", rows.get(), 2);
    add_optional_text(activity, "description", rows.get(), 5);
    add_optional_text(activity, "reason", rows.get(), 8);
    add_optional_text(activity, "traceId", rows.get(), 10);
    if (sqlite3_column_type(rows.get(), 9) != SQLITE_NULL) {
      const std::string outcome = column_text(rows.get(), 9);
      if (!is_one_of(outcome, {"success", "failure", "cancelled", "denied"})) {
        throw std::runtime_error("Stored activity has an unknown outcome");
      }
      activity["outcome"] = outcome;
    }
    activities.push_back(std::move(activity));
  }
  return {{"activities", std::move(activities)}};
}

nlohmann::json DesktopDatabase::list_tool_executions(
    const std::optional<std::string>& trace_id,
    const std::optional<std::string>& server_id, int limit) const {
  std::lock_guard lock(mutex_);
  if (limit < 1 || limit > 100) {
    throw std::invalid_argument("Tool execution limit must be between 1 and 100");
  }
  if (connection_ == nullptr) return {{"executions", nlohmann::json::array()}};

  const char* sql = trace_id.has_value()
      ? R"SQL(
          SELECT trace_id, step_id, call_id, tool_name, server_id, environment, risk_level,
                 decision, approved_by, status, input, output, error, started_at, ended_at,
                 duration_ms
            FROM tool_executions
           WHERE trace_id = ?1
           ORDER BY started_at
           LIMIT ?2
        )SQL"
      : server_id.has_value()
          ? R"SQL(
              SELECT trace_id, step_id, call_id, tool_name, server_id, environment, risk_level,
                     decision, approved_by, status, input, output, error, started_at, ended_at,
                     duration_ms
                FROM tool_executions
               WHERE server_id = ?1
               ORDER BY started_at DESC
               LIMIT ?2
            )SQL"
          : R"SQL(
              SELECT trace_id, step_id, call_id, tool_name, server_id, environment, risk_level,
                     decision, approved_by, status, input, output, error, started_at, ended_at,
                     duration_ms
                FROM tool_executions
               ORDER BY started_at DESC
               LIMIT ?1
            )SQL";
  Statement rows(connection_, sql);
  int limit_parameter = 1;
  const auto& filter = trace_id.has_value() ? trace_id : server_id;
  if (filter.has_value()) {
    const auto bind_result = sqlite3_bind_text(rows.get(), 1, filter->c_str(),
                                               static_cast<int>(filter->size()),
                                               SQLITE_TRANSIENT);
    if (bind_result != SQLITE_OK) throw std::runtime_error(sqlite3_errmsg(connection_));
    limit_parameter = 2;
  }
  if (sqlite3_bind_int(rows.get(), limit_parameter, limit) != SQLITE_OK) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }

  nlohmann::json executions = nlohmann::json::array();
  for (;;) {
    const auto result = sqlite3_step(rows.get());
    if (result == SQLITE_DONE) break;
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));

    const std::string environment = column_text(rows.get(), 5);
    const std::string risk_level = column_text(rows.get(), 6);
    const std::string decision = column_text(rows.get(), 7);
    const std::string status = column_text(rows.get(), 9);
    if (!is_one_of(environment, {"local", "development", "staging", "production", "unknown"}) ||
        !is_one_of(risk_level, {"read", "low", "medium", "high", "critical"}) ||
        !is_one_of(decision, {"auto", "ask", "deny"}) ||
        !is_one_of(status, {"pending", "running", "waiting_approval", "success", "failed",
                            "cancelled"})) {
      throw std::runtime_error("Stored tool execution has an unknown enum value");
    }

    nlohmann::json input;
    try {
      input = nlohmann::json::parse(column_text(rows.get(), 10));
    } catch (const nlohmann::json::exception&) {
      throw std::runtime_error("Stored tool execution input is invalid JSON");
    }
    nlohmann::json execution = {
        {"traceId", column_text(rows.get(), 0)},
        {"stepId", column_text(rows.get(), 1)},
        {"callId", column_text(rows.get(), 2)},
        {"toolName", column_text(rows.get(), 3)},
        {"environment", environment},
        {"riskLevel", risk_level},
        {"decision", decision},
        {"status", status},
        {"input", std::move(input)},
        {"startedAt", column_text(rows.get(), 13)},
    };
    add_optional_text(execution, "serverId", rows.get(), 4);
    add_optional_text(execution, "approvedBy", rows.get(), 8);
    add_optional_text(execution, "error", rows.get(), 12);
    add_optional_text(execution, "endedAt", rows.get(), 14);
    if (sqlite3_column_type(rows.get(), 8) != SQLITE_NULL) {
      const std::string approved_by = column_text(rows.get(), 8);
      if (!is_one_of(approved_by, {"user", "policy"})) {
        throw std::runtime_error("Stored tool execution has an unknown approval source");
      }
    }
    if (sqlite3_column_type(rows.get(), 11) != SQLITE_NULL) {
      try {
        execution["output"] = nlohmann::json::parse(column_text(rows.get(), 11));
      } catch (const nlohmann::json::exception&) {
        throw std::runtime_error("Stored tool execution output is invalid JSON");
      }
    }
    if (sqlite3_column_type(rows.get(), 15) != SQLITE_NULL) {
      const auto duration = sqlite3_column_int64(rows.get(), 15);
      if (duration < 0) throw std::runtime_error("Stored tool execution duration is invalid");
      execution["durationMs"] = static_cast<std::uint64_t>(duration);
    }
    executions.push_back(std::move(execution));
  }
  return {{"executions", std::move(executions)}};
}

nlohmann::json DesktopDatabase::list_file_transfers(
    const std::optional<std::string>& server_id, int limit) const {
  std::lock_guard lock(mutex_);
  if (limit < 1 || limit > 100) {
    throw std::invalid_argument("File transfer limit must be between 1 and 100");
  }
  if (connection_ == nullptr) return {{"transfers", nlohmann::json::array()}};

  std::vector<nlohmann::json> decoded;
  {
    const char* sql = server_id.has_value()
        ? R"SQL(
            SELECT transfer_id, server_id, direction, status, started_at_epoch_ms,
                   updated_at_epoch_ms, snapshot_json, staging_json
              FROM file_transfers
             WHERE server_id = ?1
             ORDER BY updated_at_epoch_ms DESC, transfer_id DESC
             LIMIT ?2
          )SQL"
        : R"SQL(
            SELECT transfer_id, server_id, direction, status, started_at_epoch_ms,
                   updated_at_epoch_ms, snapshot_json, staging_json
              FROM file_transfers
             ORDER BY updated_at_epoch_ms DESC, transfer_id DESC
             LIMIT ?1
          )SQL";
    Statement rows(connection_, sql);
    int limit_parameter = 1;
    if (server_id.has_value()) {
      bind_text(connection_, rows.get(), 1, *server_id);
      limit_parameter = 2;
    }
    if (sqlite3_bind_int(rows.get(), limit_parameter, limit) != SQLITE_OK) {
      throw std::runtime_error(sqlite3_errmsg(connection_));
    }
    for (;;) {
      const auto result = sqlite3_step(rows.get());
      if (result == SQLITE_DONE) break;
      if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
      decoded.push_back(decode_file_transfer(rows.get()));
    }
  }
  for (auto& snapshot : decoded) reconcile_transfer(connection_, snapshot);
  nlohmann::json transfers = nlohmann::json::array();
  for (auto& snapshot : decoded) transfers.push_back(std::move(snapshot));
  return {{"transfers", std::move(transfers)}};
}

std::optional<nlohmann::json> DesktopDatabase::get_file_transfer(
    const std::string& transfer_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) return std::nullopt;
  std::optional<nlohmann::json> snapshot;
  {
    Statement rows(connection_, R"SQL(
        SELECT transfer_id, server_id, direction, status, started_at_epoch_ms,
               updated_at_epoch_ms, snapshot_json, staging_json
          FROM file_transfers
         WHERE transfer_id = ?1
      )SQL");
    bind_text(connection_, rows.get(), 1, transfer_id);
    const auto result = sqlite3_step(rows.get());
    if (result == SQLITE_DONE) return std::nullopt;
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
    snapshot = decode_file_transfer(rows.get());
  }
  if (snapshot.has_value()) reconcile_transfer(connection_, *snapshot);
  return snapshot;
}

nlohmann::json DesktopDatabase::list_mcp_servers() const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) return {{"servers", nlohmann::json::array()}};
  Statement rows(connection_, R"SQL(
      SELECT id, label, transport, command, args, url, enabled, allowed_tools, trust_level,
             http_auth_headers, oauth, annotation_trust
        FROM mcp_servers
       ORDER BY label COLLATE NOCASE, id
    )SQL");
  nlohmann::json servers = nlohmann::json::array();
  for (;;) {
    const auto result = sqlite3_step(rows.get());
    if (result == SQLITE_DONE) break;
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
    servers.push_back(decode_mcp_server(rows.get()));
  }
  return {{"servers", std::move(servers)}};
}

std::optional<nlohmann::json> DesktopDatabase::get_mcp_server(
    const std::string& server_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) return std::nullopt;
  Statement rows(connection_, R"SQL(
      SELECT id, label, transport, command, args, url, enabled, allowed_tools, trust_level,
             http_auth_headers, oauth, annotation_trust
        FROM mcp_servers
       WHERE id = ?1
    )SQL");
  bind_text(connection_, rows.get(), 1, server_id);
  const auto result = sqlite3_step(rows.get());
  if (result == SQLITE_DONE) return std::nullopt;
  if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
  return decode_mcp_server(rows.get());
}

void DesktopDatabase::upsert_mcp_server(const nlohmann::json& config) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local MCP database is unavailable");
  Statement statement(connection_, R"SQL(
      INSERT INTO mcp_servers (
        id, label, transport, command, args, url, enabled, allowed_tools, trust_level,
        http_auth_headers, oauth, annotation_trust
      ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
      ON CONFLICT(id) DO UPDATE SET
        label = excluded.label, transport = excluded.transport, command = excluded.command,
        args = excluded.args, url = excluded.url, enabled = excluded.enabled,
        allowed_tools = excluded.allowed_tools, trust_level = excluded.trust_level,
        http_auth_headers = excluded.http_auth_headers, oauth = excluded.oauth,
        annotation_trust = excluded.annotation_trust
    )SQL");
  bind_text(connection_, statement.get(), 1, config.at("id").get<std::string>());
  bind_text(connection_, statement.get(), 2, config.at("label").get<std::string>());
  bind_text(connection_, statement.get(), 3, config.at("transport").get<std::string>());
  std::optional<std::string> command;
  if (config.contains("command") && config.at("command").is_string()) {
    command = config.at("command").get<std::string>();
  }
  bind_optional_text(connection_, statement.get(), 4, command);
  std::optional<std::string> args;
  if (config.contains("args")) args = config.at("args").dump();
  bind_optional_text(connection_, statement.get(), 5, args);
  std::optional<std::string> url;
  if (config.contains("url") && config.at("url").is_string()) {
    url = config.at("url").get<std::string>();
  }
  bind_optional_text(connection_, statement.get(), 6, url);
  if (sqlite3_bind_int(statement.get(), 7, config.value("enabled", false) ? 1 : 0) != SQLITE_OK) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  bind_text(connection_, statement.get(), 8,
            config.value("allowedTools", nlohmann::json::array()).dump());
  bind_text(connection_, statement.get(), 9, config.at("trustLevel").get<std::string>());
  bind_text(connection_, statement.get(), 10,
            config.value("httpAuthHeaders", nlohmann::json::array()).dump());
  std::optional<std::string> oauth;
  if (config.contains("oauth")) oauth = config.at("oauth").dump();
  bind_optional_text(connection_, statement.get(), 11, oauth);
  bind_text(connection_, statement.get(), 12,
            config.value("annotationTrust", std::string("none")));
  if (sqlite3_step(statement.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
}

std::optional<nlohmann::json> DesktopDatabase::delete_mcp_server(
    const std::string& server_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local MCP database is unavailable");
  auto existing = get_mcp_server(server_id);
  if (!existing.has_value()) return std::nullopt;
  Statement statement(connection_, "DELETE FROM mcp_servers WHERE id = ?1");
  bind_text(connection_, statement.get(), 1, server_id);
  if (sqlite3_step(statement.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  return existing;
}

void DesktopDatabase::insert_snapshot(const nlohmann::json& snapshot) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) {
    throw std::runtime_error("The local snapshot database is unavailable");
  }
  const auto health = snapshot.at("health").get<std::string>();
  if (!is_one_of(health, {"healthy", "warning", "critical", "unknown"})) {
    throw std::runtime_error("The snapshot health is invalid");
  }
  Statement statement(connection_, R"SQL(
      INSERT INTO snapshots (id, server_id, collected_at, health, payload)
      VALUES (?1, ?2, ?3, ?4, ?5)
    )SQL");
  bind_text(connection_, statement.get(), 1, snapshot.at("id").get<std::string>());
  bind_text(connection_, statement.get(), 2, snapshot.at("serverId").get<std::string>());
  bind_text(connection_, statement.get(), 3, snapshot.at("collectedAt").get<std::string>());
  bind_text(connection_, statement.get(), 4, health);
  bind_text(connection_, statement.get(), 5, snapshot.dump());
  if (sqlite3_step(statement.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
}

nlohmann::json DesktopDatabase::list_investigation_tasks(
    const std::optional<std::string>& status, int limit) const {
  std::lock_guard lock(mutex_);
  if (limit < 1 || limit > 100) {
    throw std::invalid_argument("Investigation task limit must be between 1 and 100");
  }
  if (connection_ == nullptr) return {{"tasks", nlohmann::json::array()}};
  const char* sql = status.has_value()
      ? R"SQL(
          SELECT id, workspace_id, server_id, objective, success_criteria, scope, mode,
                 permission_mode, automation_level, status, max_steps, max_run_ms,
                 max_attempts, created_by, phase, created_at, updated_at, completed_at,
                 active_run_id, last_failure, guardrails
            FROM investigation_tasks
           WHERE status = ?1
           ORDER BY updated_at DESC
           LIMIT ?2
        )SQL"
      : R"SQL(
          SELECT id, workspace_id, server_id, objective, success_criteria, scope, mode,
                 permission_mode, automation_level, status, max_steps, max_run_ms,
                 max_attempts, created_by, phase, created_at, updated_at, completed_at,
                 active_run_id, last_failure, guardrails
            FROM investigation_tasks
           ORDER BY updated_at DESC
           LIMIT ?1
        )SQL";
  Statement rows(connection_, sql);
  int limit_parameter = 1;
  if (status.has_value()) {
    bind_text(connection_, rows.get(), 1, *status);
    limit_parameter = 2;
  }
  if (sqlite3_bind_int(rows.get(), limit_parameter, limit) != SQLITE_OK) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  nlohmann::json tasks = nlohmann::json::array();
  for (;;) {
    const auto result = sqlite3_step(rows.get());
    if (result == SQLITE_DONE) break;
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
    tasks.push_back(decode_investigation_task(rows.get()));
  }
  return {{"tasks", std::move(tasks)}};
}

std::optional<nlohmann::json> DesktopDatabase::get_investigation_task(
    const std::string& task_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) return std::nullopt;
  nlohmann::json task;
  {
    Statement rows(connection_, R"SQL(
        SELECT id, workspace_id, server_id, objective, success_criteria, scope, mode,
               permission_mode, automation_level, status, max_steps, max_run_ms,
               max_attempts, created_by, phase, created_at, updated_at, completed_at,
               active_run_id, last_failure, guardrails
          FROM investigation_tasks
         WHERE id = ?1
      )SQL");
    bind_text(connection_, rows.get(), 1, task_id);
    const auto result = sqlite3_step(rows.get());
    if (result == SQLITE_DONE) return std::nullopt;
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
    task = decode_investigation_task(rows.get());
  }
  const auto collect = [&](const char* sql,
                           nlohmann::json (*decode)(sqlite3_stmt*)) {
    nlohmann::json items = nlohmann::json::array();
    Statement rows(connection_, sql);
    bind_text(connection_, rows.get(), 1, task_id);
    for (;;) {
      const auto result = sqlite3_step(rows.get());
      if (result == SQLITE_DONE) break;
      if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
      items.push_back(decode(rows.get()));
    }
    return items;
  };
  auto evidence = collect(
      R"SQL(
        SELECT id, task_id, scope, kind, source_tool, collected_at, input_summary,
               content_type, content, content_hash, truncated, redaction_status, run_id
          FROM investigation_evidence
         WHERE task_id = ?1
         ORDER BY collected_at DESC, id
      )SQL",
      decode_investigation_evidence);
  auto findings = collect(
      R"SQL(
        SELECT id, task_id, title, kind, statement, evidence_ids, confidence,
               next_verification, created_at
          FROM investigation_findings
         WHERE task_id = ?1
         ORDER BY created_at DESC, id
      )SQL",
      decode_investigation_finding);
  auto artifacts = collect(
      R"SQL(
        SELECT id, task_id, run_id, phase, kind, status, title, summary, content,
               evidence_ids, created_at, updated_at, plan_id, plan_step_id
          FROM investigation_artifacts
         WHERE task_id = ?1
         ORDER BY updated_at DESC, id DESC
      )SQL",
      decode_investigation_artifact);
  auto runs = collect(
      R"SQL(
        SELECT id, task_id, session_id, message_id, trace_id, attempt, phase, status,
               started_at, updated_at, ended_at, checkpoint, failure
          FROM investigation_runs
         WHERE task_id = ?1
         ORDER BY updated_at DESC, id
      )SQL",
      decode_investigation_run);
  auto steps = collect(
      R"SQL(
        SELECT id, task_id, run_id, ordinal, kind, title, status, attempt, tool_name,
               target, input_summary, output_summary, evidence_ids, started_at, ended_at,
               failure, plan_id, plan_step_id
          FROM investigation_steps
         WHERE task_id = ?1
         ORDER BY ordinal, id
      )SQL",
      decode_investigation_step);

  std::optional<nlohmann::json> brief;
  {
    Statement rows(connection_, R"SQL(
        SELECT id, task_id, generated_at, status, finding_ids, options, selected_option_id,
               plan_id
          FROM investigation_decision_briefs
         WHERE task_id = ?1
         ORDER BY generated_at DESC, id DESC
         LIMIT 1
      )SQL");
    bind_text(connection_, rows.get(), 1, task_id);
    const auto result = sqlite3_step(rows.get());
    if (result == SQLITE_ROW) brief = decode_investigation_brief(rows.get());
    else if (result != SQLITE_DONE) throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  std::optional<nlohmann::json> plan;
  {
    Statement rows(connection_, R"SQL(
        SELECT id, task_id, revision, status, created_at, updated_at, current_step_id, steps,
               approval, observation_window
          FROM investigation_plans
         WHERE task_id = ?1
         ORDER BY (status IN ('draft','active')) DESC, revision DESC, id DESC
         LIMIT 1
      )SQL");
    bind_text(connection_, rows.get(), 1, task_id);
    const auto result = sqlite3_step(rows.get());
    if (result == SQLITE_ROW) plan = decode_investigation_plan(rows.get());
    else if (result != SQLITE_DONE) throw std::runtime_error(sqlite3_errmsg(connection_));
  }

  nlohmann::json detail = {{"task", std::move(task)},
                           {"evidence", std::move(evidence)},
                           {"findings", std::move(findings)},
                           {"artifacts", std::move(artifacts)},
                           {"runs", std::move(runs)},
                           {"steps", std::move(steps)}};
  if (brief.has_value()) detail["decisionBrief"] = std::move(*brief);
  if (plan.has_value()) detail["plan"] = std::move(*plan);
  return detail;
}

std::optional<nlohmann::json> DesktopDatabase::set_investigation_task_status(
    const std::string& task_id, const std::string& status,
    const std::optional<std::string>& completed_at, const std::string& updated_at) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) return std::nullopt;
  Statement update(connection_, R"SQL(
      UPDATE investigation_tasks
         SET status = ?2, completed_at = ?3, updated_at = ?4
       WHERE id = ?1
    )SQL");
  bind_text(connection_, update.get(), 1, task_id);
  bind_text(connection_, update.get(), 2, status);
  bind_optional_text(connection_, update.get(), 3, completed_at);
  bind_text(connection_, update.get(), 4, updated_at);
  if (sqlite3_step(update.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  if (sqlite3_changes(connection_) == 0) return std::nullopt;
  Statement rows(connection_, R"SQL(
      SELECT id, workspace_id, server_id, objective, success_criteria, scope, mode,
             permission_mode, automation_level, status, max_steps, max_run_ms,
             max_attempts, created_by, phase, created_at, updated_at, completed_at,
             active_run_id, last_failure, guardrails
        FROM investigation_tasks
       WHERE id = ?1
    )SQL");
  bind_text(connection_, rows.get(), 1, task_id);
  const auto result = sqlite3_step(rows.get());
  if (result == SQLITE_DONE) return std::nullopt;
  if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
  return decode_investigation_task(rows.get());
}

std::optional<std::string> DesktopDatabase::active_investigation_run_id(
    const std::string& task_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) return std::nullopt;
  Statement rows(connection_, "SELECT active_run_id FROM investigation_tasks WHERE id = ?1");
  bind_text(connection_, rows.get(), 1, task_id);
  const auto result = sqlite3_step(rows.get());
  if (result == SQLITE_DONE) return std::nullopt;
  if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
  if (sqlite3_column_type(rows.get(), 0) == SQLITE_NULL) return std::nullopt;
  return std::string(column_text(rows.get(), 0));
}

std::string DesktopDatabase::content_hash(const nlohmann::json& content) {
  return sha256_hex(content.dump());
}

std::optional<nlohmann::json> DesktopDatabase::create_investigation_task(
    const nlohmann::json& task) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) return std::nullopt;
  const auto field = [&task](const char* key) {
    return task.value(key, std::string());
  };
  if (field("id").empty()) throw std::invalid_argument("investigation task id is required");
  Statement insert(connection_, R"SQL(
      INSERT INTO investigation_tasks (
        id, workspace_id, server_id, objective, success_criteria, scope, mode,
        permission_mode, automation_level, status, max_steps, max_run_ms, completed_at,
        max_attempts, created_by, phase, active_run_id, last_failure, guardrails,
        created_at, updated_at
      ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19,
                ?20, ?21)
    )SQL");
  const auto text_or_null = [&task](const char* key) -> std::optional<std::string> {
    const auto found = task.find(key);
    if (found == task.end() || !found->is_string()) return std::nullopt;
    return found->get<std::string>();
  };
  bind_text(connection_, insert.get(), 1, field("id"));
  bind_optional_text(connection_, insert.get(), 2, text_or_null("workspaceId"));
  bind_optional_text(connection_, insert.get(), 3, text_or_null("serverId"));
  bind_text(connection_, insert.get(), 4, task.value("objective", std::string()));
  bind_text(connection_, insert.get(), 5,
            task.contains("successCriteria") ? task.at("successCriteria").dump() : "[]");
  bind_text(connection_, insert.get(), 6,
            task.contains("scope") ? task.at("scope").dump() : "{}");
  bind_text(connection_, insert.get(), 7, task.value("mode", std::string("goal")));
  bind_text(connection_, insert.get(), 8, task.value("permissionMode", std::string("ask")));
  bind_text(connection_, insert.get(), 9, task.value("automationLevel", std::string("propose")));
  bind_text(connection_, insert.get(), 10, task.value("status", std::string("pending")));
  const auto budget = task.contains("budget") && task.at("budget").is_object()
                          ? task.at("budget")
                          : nlohmann::json::object();
  bind_text(connection_, insert.get(), 11,
            std::to_string(budget.value("maxSteps", std::int64_t{50})));
  bind_text(connection_, insert.get(), 12,
            std::to_string(budget.value("maxRunMs", std::int64_t{600000})));
  bind_optional_text(connection_, insert.get(), 13, text_or_null("completedAt"));
  bind_text(connection_, insert.get(), 14,
            std::to_string(budget.value("maxAttempts", std::int64_t{3})));
  bind_text(connection_, insert.get(), 15, task.value("createdBy", std::string("user")));
  bind_text(connection_, insert.get(), 16, task.value("phase", std::string("investigating")));
  bind_optional_text(connection_, insert.get(), 17, text_or_null("activeRunId"));
  const auto failure = task.find("lastFailure");
  if (failure != task.end() && !failure->is_null()) {
    bind_text(connection_, insert.get(), 18, failure->dump());
  } else {
    bind_optional_text(connection_, insert.get(), 18, std::nullopt);
  }
  bind_text(connection_, insert.get(), 19,
            task.contains("guardrails") ? task.at("guardrails").dump() : "{}");
  const auto created_at = text_or_null("createdAt");
  const auto updated_at = text_or_null("updatedAt");
  if (!created_at.has_value() || !updated_at.has_value()) {
    throw std::invalid_argument("investigation task createdAt and updatedAt are required");
  }
  bind_text(connection_, insert.get(), 20, *created_at);
  bind_text(connection_, insert.get(), 21, *updated_at);
  if (sqlite3_step(insert.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  return load_investigation_task_row(connection_, field("id"));
}

std::optional<nlohmann::json> DesktopDatabase::update_investigation_task_progress(
    const std::string& task_id, const std::string& status, const std::string& phase,
    const std::optional<std::string>& active_run_id,
    const std::optional<nlohmann::json>& last_failure, const std::string& updated_at,
    const std::optional<std::string>& completed_at) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) return std::nullopt;
  Statement update(connection_, R"SQL(
      UPDATE investigation_tasks
         SET status = ?2, phase = ?3, active_run_id = ?4, last_failure = ?5,
             updated_at = ?6, completed_at = ?7
       WHERE id = ?1
    )SQL");
  bind_text(connection_, update.get(), 1, task_id);
  bind_text(connection_, update.get(), 2, status);
  bind_text(connection_, update.get(), 3, phase);
  bind_optional_text(connection_, update.get(), 4, active_run_id);
  if (last_failure.has_value()) {
    bind_text(connection_, update.get(), 5, last_failure->dump());
  } else {
    bind_optional_text(connection_, update.get(), 5, std::nullopt);
  }
  bind_text(connection_, update.get(), 6, updated_at);
  bind_optional_text(connection_, update.get(), 7, completed_at);
  if (sqlite3_step(update.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  if (sqlite3_changes(connection_) == 0) return std::nullopt;
  return load_investigation_task_row(connection_, task_id);
}

std::optional<nlohmann::json> DesktopDatabase::get_investigation_plan(
    const std::string& plan_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) return std::nullopt;
  Statement rows(connection_, R"SQL(
      SELECT id, task_id, revision, status, created_at, updated_at, current_step_id, steps,
             approval, observation_window
        FROM investigation_plans
       WHERE id = ?1
    )SQL");
  bind_text(connection_, rows.get(), 1, plan_id);
  const auto result = sqlite3_step(rows.get());
  if (result == SQLITE_DONE) return std::nullopt;
  if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
  return decode_investigation_plan(rows.get());
}

std::optional<nlohmann::json> DesktopDatabase::latest_investigation_plan(
    const std::string& task_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) return std::nullopt;
  Statement rows(connection_, R"SQL(
      SELECT id, task_id, revision, status, created_at, updated_at, current_step_id, steps,
             approval, observation_window
        FROM investigation_plans
       WHERE task_id = ?1 AND status IN ('draft','active','completed')
       ORDER BY revision DESC, id DESC
       LIMIT 1
    )SQL");
  bind_text(connection_, rows.get(), 1, task_id);
  const auto result = sqlite3_step(rows.get());
  if (result == SQLITE_DONE) return std::nullopt;
  if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
  return decode_investigation_plan(rows.get());
}

void DesktopDatabase::save_investigation_plan(const nlohmann::json& plan) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("the local database is unavailable");
  const auto plan_id = plan.value("id", std::string());
  const auto task_id = plan.value("taskId", std::string());
  if (plan_id.empty() || task_id.empty()) {
    throw std::invalid_argument("investigation plan id and taskId are required");
  }
  const auto steps = plan.contains("steps") ? plan.at("steps").dump() : "[]";
  const auto approval =
      plan.contains("approval") && !plan.at("approval").is_null()
          ? std::optional<std::string>(plan.at("approval").dump())
          : std::nullopt;
  const auto window =
      plan.contains("observationWindow") && !plan.at("observationWindow").is_null()
          ? std::optional<std::string>(plan.at("observationWindow").dump())
          : std::nullopt;
  Transaction transaction(connection_);
  {
    Statement existing(connection_, "SELECT task_id FROM investigation_plans WHERE id = ?1");
    bind_text(connection_, existing.get(), 1, plan_id);
    const auto result = sqlite3_step(existing.get());
    if (result == SQLITE_ROW) {
      if (std::string(column_text(existing.get(), 0)) != task_id) {
        throw std::invalid_argument("investigation plan id belongs to another task");
      }
      Statement update(connection_, R"SQL(
          UPDATE investigation_plans
             SET revision = ?2, status = ?3, created_at = ?4, updated_at = ?5,
                 current_step_id = ?6, steps = ?7, approval = ?8, observation_window = ?9
           WHERE id = ?1
        )SQL");
      bind_text(connection_, update.get(), 1, plan_id);
      if (sqlite3_bind_int64(update.get(), 2, plan.value("revision", std::int64_t{1})) != SQLITE_OK) {
        throw std::runtime_error(sqlite3_errmsg(connection_));
      }
      bind_text(connection_, update.get(), 3, plan.value("status", std::string("active")));
      bind_text(connection_, update.get(), 4, plan.value("createdAt", std::string()));
      bind_text(connection_, update.get(), 5, plan.value("updatedAt", std::string()));
      const auto current = plan.find("currentStepId");
      if (current != plan.end() && current->is_string()) {
        bind_text(connection_, update.get(), 6, current->get<std::string>());
      } else {
        bind_optional_text(connection_, update.get(), 6, std::nullopt);
      }
      bind_text(connection_, update.get(), 7, steps);
      bind_optional_text(connection_, update.get(), 8, approval);
      bind_optional_text(connection_, update.get(), 9, window);
      if (sqlite3_step(update.get()) != SQLITE_DONE) {
        throw std::runtime_error(sqlite3_errmsg(connection_));
      }
    } else if (result == SQLITE_DONE) {
      Statement supersede(connection_, R"SQL(
          UPDATE investigation_plans SET status = 'superseded', updated_at = ?2
           WHERE task_id = ?1 AND status IN ('draft','active')
        )SQL");
      bind_text(connection_, supersede.get(), 1, task_id);
      bind_text(connection_, supersede.get(), 2, plan.value("updatedAt", std::string()));
      if (sqlite3_step(supersede.get()) != SQLITE_DONE) {
        throw std::runtime_error(sqlite3_errmsg(connection_));
      }
      Statement insert(connection_, R"SQL(
          INSERT INTO investigation_plans
            (id, task_id, revision, status, created_at, updated_at, current_step_id, steps,
             approval, observation_window)
          VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
        )SQL");
      bind_text(connection_, insert.get(), 1, plan_id);
      bind_text(connection_, insert.get(), 2, task_id);
      if (sqlite3_bind_int64(insert.get(), 3, plan.value("revision", std::int64_t{1})) != SQLITE_OK) {
        throw std::runtime_error(sqlite3_errmsg(connection_));
      }
      bind_text(connection_, insert.get(), 4, plan.value("status", std::string("active")));
      bind_text(connection_, insert.get(), 5, plan.value("createdAt", std::string()));
      bind_text(connection_, insert.get(), 6, plan.value("updatedAt", std::string()));
      const auto current = plan.find("currentStepId");
      if (current != plan.end() && current->is_string()) {
        bind_text(connection_, insert.get(), 7, current->get<std::string>());
      } else {
        bind_optional_text(connection_, insert.get(), 7, std::nullopt);
      }
      bind_text(connection_, insert.get(), 8, steps);
      bind_optional_text(connection_, insert.get(), 9, approval);
      bind_optional_text(connection_, insert.get(), 10, window);
      if (sqlite3_step(insert.get()) != SQLITE_DONE) {
        throw std::runtime_error(sqlite3_errmsg(connection_));
      }
    } else {
      throw std::runtime_error(sqlite3_errmsg(connection_));
    }
  }
  transaction.commit();
}

void DesktopDatabase::upsert_investigation_artifact(const nlohmann::json& artifact) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("the local database is unavailable");
  const auto artifact_id = artifact.value("id", std::string());
  const auto task_id = artifact.value("taskId", std::string());
  if (artifact_id.empty() || task_id.empty()) {
    throw std::invalid_argument("investigation artifact id and taskId are required");
  }
  const auto content = artifact.contains("content") ? artifact.at("content").dump() : "null";
  if (content.size() > 1048576) {
    throw std::invalid_argument("investigation artifact content exceeds the 1 MiB limit");
  }
  {
    Statement existing(connection_,
                       "SELECT task_id FROM investigation_artifacts WHERE id = ?1");
    bind_text(connection_, existing.get(), 1, artifact_id);
    const auto result = sqlite3_step(existing.get());
    if (result == SQLITE_ROW &&
        std::string(column_text(existing.get(), 0)) != task_id) {
      throw std::invalid_argument("investigation artifact id belongs to another task");
    }
    if (result != SQLITE_ROW && result != SQLITE_DONE) {
      throw std::runtime_error(sqlite3_errmsg(connection_));
    }
  }
  const auto optional_text = [&artifact](const char* key) -> std::optional<std::string> {
    const auto found = artifact.find(key);
    if (found == artifact.end() || !found->is_string()) return std::nullopt;
    return found->get<std::string>();
  };
  Statement upsert(connection_, R"SQL(
      INSERT INTO investigation_artifacts (
        id, task_id, run_id, plan_id, plan_step_id, phase, kind, status, title, summary,
        content, evidence_ids, created_at, updated_at
      ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
      ON CONFLICT(id) DO UPDATE SET
        run_id = excluded.run_id, plan_id = excluded.plan_id,
        plan_step_id = excluded.plan_step_id, phase = excluded.phase, kind = excluded.kind,
        status = excluded.status, title = excluded.title, summary = excluded.summary,
        content = excluded.content, evidence_ids = excluded.evidence_ids,
        updated_at = excluded.updated_at
    )SQL");
  bind_text(connection_, upsert.get(), 1, artifact_id);
  bind_text(connection_, upsert.get(), 2, task_id);
  bind_optional_text(connection_, upsert.get(), 3, optional_text("runId"));
  bind_optional_text(connection_, upsert.get(), 4, optional_text("planId"));
  bind_optional_text(connection_, upsert.get(), 5, optional_text("planStepId"));
  bind_text(connection_, upsert.get(), 6, artifact.value("phase", std::string("investigating")));
  bind_text(connection_, upsert.get(), 7, artifact.value("kind", std::string()));
  bind_text(connection_, upsert.get(), 8, artifact.value("status", std::string("draft")));
  bind_text(connection_, upsert.get(), 9, artifact.value("title", std::string()));
  bind_text(connection_, upsert.get(), 10, artifact.value("summary", std::string()));
  bind_text(connection_, upsert.get(), 11, content);
  bind_text(connection_, upsert.get(), 12,
            artifact.contains("evidenceIds") ? artifact.at("evidenceIds").dump() : "[]");
  bind_text(connection_, upsert.get(), 13, artifact.value("createdAt", std::string()));
  bind_text(connection_, upsert.get(), 14, artifact.value("updatedAt", std::string()));
  if (sqlite3_step(upsert.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
}

nlohmann::json DesktopDatabase::list_investigation_artifacts(const std::string& task_id,
                                                            int limit) const {
  std::lock_guard lock(mutex_);
  nlohmann::json items = nlohmann::json::array();
  if (connection_ == nullptr) return {{"artifacts", std::move(items)}};
  Statement rows(connection_, R"SQL(
      SELECT id, task_id, run_id, phase, kind, status, title, summary, content, evidence_ids,
             created_at, updated_at, plan_id, plan_step_id
        FROM investigation_artifacts
       WHERE task_id = ?1
       ORDER BY updated_at DESC, id DESC
       LIMIT ?2
    )SQL");
  bind_text(connection_, rows.get(), 1, task_id);
  if (sqlite3_bind_int(rows.get(), 2, limit) != SQLITE_OK) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  for (;;) {
    const auto result = sqlite3_step(rows.get());
    if (result == SQLITE_DONE) break;
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
    items.push_back(decode_investigation_artifact(rows.get()));
  }
  return {{"artifacts", std::move(items)}};
}

std::optional<nlohmann::json> DesktopDatabase::find_investigation_evidence_in_run(
    const nlohmann::json& evidence) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) return std::nullopt;
  const auto run_id = evidence.find("runId");
  if (run_id == evidence.end() || !run_id->is_string()) return std::nullopt;
  Statement rows(connection_, R"SQL(
      SELECT id, task_id, scope, kind, source_tool, collected_at, input_summary, content_type,
             content, content_hash, truncated, redaction_status, run_id
        FROM investigation_evidence
       WHERE task_id = ?1 AND run_id = ?2 AND scope = ?3 AND kind = ?4 AND source_tool = ?5
         AND input_summary = ?6 AND content_hash = ?7 AND truncated = ?8 AND content_type = ?9
       ORDER BY collected_at DESC, id DESC
       LIMIT 1
    )SQL");
  bind_text(connection_, rows.get(), 1, evidence.value("taskId", std::string()));
  bind_text(connection_, rows.get(), 2, run_id->get<std::string>());
  bind_text(connection_, rows.get(), 3, evidence.at("scope").dump());
  bind_text(connection_, rows.get(), 4, evidence.value("kind", std::string()));
  bind_text(connection_, rows.get(), 5, evidence.value("sourceTool", std::string()));
  bind_text(connection_, rows.get(), 6, evidence.value("inputSummary", std::string()));
  bind_text(connection_, rows.get(), 7, evidence.value("contentHash", std::string()));
  if (sqlite3_bind_int(rows.get(), 8, evidence.value("truncated", false) ? 1 : 0) != SQLITE_OK) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  bind_text(connection_, rows.get(), 9, evidence.value("contentType", std::string()));
  const auto result = sqlite3_step(rows.get());
  if (result == SQLITE_DONE) return std::nullopt;
  if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
  return decode_investigation_evidence(rows.get());
}

void DesktopDatabase::record_investigation_evidence(const nlohmann::json& evidence) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("the local database is unavailable");
  const auto content = evidence.contains("content") ? evidence.at("content").dump() : "null";
  if (content.size() > 1048576) {
    throw std::invalid_argument("evidence content exceeds 1048576 bytes");
  }
  if (evidence.value("redactionStatus", std::string("unknown")) == "unknown") {
    throw std::invalid_argument("evidence must be redacted before persistence");
  }
  if (content_hash(evidence.at("content")) != evidence.value("contentHash", std::string())) {
    throw std::invalid_argument("evidence contentHash does not match content");
  }
  Statement insert(connection_, R"SQL(
      INSERT INTO investigation_evidence (
        id, task_id, run_id, scope, kind, source_tool, collected_at, input_summary,
        content_type, content, content_hash, truncated, redaction_status
      ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
    )SQL");
  bind_text(connection_, insert.get(), 1, evidence.value("id", std::string()));
  bind_text(connection_, insert.get(), 2, evidence.value("taskId", std::string()));
  const auto run_id = evidence.find("runId");
  if (run_id != evidence.end() && run_id->is_string()) {
    bind_text(connection_, insert.get(), 3, run_id->get<std::string>());
  } else {
    bind_optional_text(connection_, insert.get(), 3, std::nullopt);
  }
  bind_text(connection_, insert.get(), 4, evidence.at("scope").dump());
  bind_text(connection_, insert.get(), 5, evidence.value("kind", std::string()));
  bind_text(connection_, insert.get(), 6, evidence.value("sourceTool", std::string()));
  bind_text(connection_, insert.get(), 7, evidence.value("collectedAt", std::string()));
  bind_text(connection_, insert.get(), 8, evidence.value("inputSummary", std::string()));
  bind_text(connection_, insert.get(), 9, evidence.value("contentType", std::string()));
  bind_text(connection_, insert.get(), 10, content);
  bind_text(connection_, insert.get(), 11, evidence.value("contentHash", std::string()));
  if (sqlite3_bind_int(insert.get(), 12, evidence.value("truncated", false) ? 1 : 0) != SQLITE_OK) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  bind_text(connection_, insert.get(), 13, evidence.value("redactionStatus", std::string()));
  if (sqlite3_step(insert.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
}

std::optional<nlohmann::json> DesktopDatabase::get_investigation_evidence(
    const std::string& evidence_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) return std::nullopt;
  Statement rows(connection_, R"SQL(
      SELECT id, task_id, scope, kind, source_tool, collected_at, input_summary, content_type,
             content, content_hash, truncated, redaction_status, run_id
        FROM investigation_evidence
       WHERE id = ?1
    )SQL");
  bind_text(connection_, rows.get(), 1, evidence_id);
  const auto result = sqlite3_step(rows.get());
  if (result == SQLITE_DONE) return std::nullopt;
  if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
  return decode_investigation_evidence(rows.get());
}

nlohmann::json DesktopDatabase::search_investigation_evidence(
    const std::string& task_id, const nlohmann::json& query) const {
  std::lock_guard lock(mutex_);
  nlohmann::json items = nlohmann::json::array();
  if (connection_ == nullptr) return std::move(items);
  const auto optional = [&query](const char* key) -> std::optional<std::string> {
    const auto found = query.find(key);
    if (found == query.end() || !found->is_string()) return std::nullopt;
    return found->get<std::string>();
  };
  const auto limit = query.value("limit", 32);
  if (limit < 1 || limit > 64) {
    throw std::invalid_argument("evidence search limit must be between 1 and 64");
  }
  const auto scope = query.find("scope");
  const std::optional<std::string> scope_text =
      scope != query.end() && scope->is_object() ? std::optional<std::string>(scope->dump())
                                                 : std::nullopt;
  Statement rows(connection_, R"SQL(
      SELECT id, task_id, scope, kind, source_tool, collected_at, input_summary, content_type,
             content, content_hash, truncated, redaction_status, run_id
        FROM investigation_evidence
       WHERE task_id = ?1
         AND (?2 IS NULL OR source_tool = ?2)
         AND (?3 IS NULL OR kind = ?3)
         AND (?4 IS NULL OR collected_at >= ?4)
         AND (?5 IS NULL OR collected_at <= ?5)
         AND (?6 IS NULL OR scope = ?6)
       ORDER BY collected_at DESC, id DESC
       LIMIT ?7
    )SQL");
  bind_text(connection_, rows.get(), 1, task_id);
  bind_optional_text(connection_, rows.get(), 2, optional("sourceTool"));
  bind_optional_text(connection_, rows.get(), 3, optional("kind"));
  bind_optional_text(connection_, rows.get(), 4, optional("from"));
  bind_optional_text(connection_, rows.get(), 5, optional("to"));
  bind_optional_text(connection_, rows.get(), 6, scope_text);
  if (sqlite3_bind_int(rows.get(), 7, limit) != SQLITE_OK) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  for (;;) {
    const auto result = sqlite3_step(rows.get());
    if (result == SQLITE_DONE) break;
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
    items.push_back(decode_investigation_evidence(rows.get()));
  }
  return items;
}

std::optional<nlohmann::json> DesktopDatabase::get_investigation_finding(
    const std::string& finding_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) return std::nullopt;
  Statement rows(connection_, R"SQL(
      SELECT id, task_id, title, kind, statement, evidence_ids, confidence, next_verification,
             created_at
        FROM investigation_findings
       WHERE id = ?1
    )SQL");
  bind_text(connection_, rows.get(), 1, finding_id);
  const auto result = sqlite3_step(rows.get());
  if (result == SQLITE_DONE) return std::nullopt;
  if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
  return decode_investigation_finding(rows.get());
}

void DesktopDatabase::record_investigation_finding(const nlohmann::json& finding) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("the local database is unavailable");
  Statement insert(connection_, R"SQL(
      INSERT INTO investigation_findings (
        id, task_id, title, kind, statement, evidence_ids, confidence, next_verification,
        created_at
      ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
    )SQL");
  bind_text(connection_, insert.get(), 1, finding.value("id", std::string()));
  bind_text(connection_, insert.get(), 2, finding.value("taskId", std::string()));
  bind_text(connection_, insert.get(), 3, finding.value("title", std::string()));
  bind_text(connection_, insert.get(), 4, finding.value("kind", std::string("unknown")));
  bind_text(connection_, insert.get(), 5, finding.value("statement", std::string()));
  bind_text(connection_, insert.get(), 6,
            finding.contains("evidenceIds") ? finding.at("evidenceIds").dump() : "[]");
  bind_text(connection_, insert.get(), 7, finding.value("confidence", std::string("low")));
  const auto next = finding.find("nextVerification");
  if (next != finding.end() && next->is_string()) {
    bind_text(connection_, insert.get(), 8, next->get<std::string>());
  } else {
    bind_optional_text(connection_, insert.get(), 8, std::nullopt);
  }
  bind_text(connection_, insert.get(), 9, finding.value("createdAt", std::string()));
  if (sqlite3_step(insert.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
}

std::optional<nlohmann::json> DesktopDatabase::get_investigation_brief(
    const std::string& brief_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) return std::nullopt;
  Statement rows(connection_, R"SQL(
      SELECT id, task_id, generated_at, status, finding_ids, options, selected_option_id, plan_id
        FROM investigation_decision_briefs
       WHERE id = ?1
    )SQL");
  bind_text(connection_, rows.get(), 1, brief_id);
  const auto result = sqlite3_step(rows.get());
  if (result == SQLITE_DONE) return std::nullopt;
  if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
  return decode_investigation_brief(rows.get());
}

void DesktopDatabase::record_investigation_brief(const nlohmann::json& brief) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("the local database is unavailable");
  Statement insert(connection_, R"SQL(
      INSERT INTO investigation_decision_briefs (
        id, task_id, plan_id, generated_at, status, finding_ids, options, selected_option_id
      ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
    )SQL");
  bind_text(connection_, insert.get(), 1, brief.value("id", std::string()));
  bind_text(connection_, insert.get(), 2, brief.value("taskId", std::string()));
  const auto plan_id = brief.find("planId");
  if (plan_id != brief.end() && plan_id->is_string()) {
    bind_text(connection_, insert.get(), 3, plan_id->get<std::string>());
  } else {
    bind_optional_text(connection_, insert.get(), 3, std::nullopt);
  }
  bind_text(connection_, insert.get(), 4, brief.value("generatedAt", std::string()));
  bind_text(connection_, insert.get(), 5, brief.value("status", std::string("draft")));
  bind_text(connection_, insert.get(), 6,
            brief.contains("findingIds") ? brief.at("findingIds").dump() : "[]");
  bind_text(connection_, insert.get(), 7,
            brief.contains("options") ? brief.at("options").dump() : "[]");
  bind_optional_text(connection_, insert.get(), 8, std::nullopt);
  if (sqlite3_step(insert.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
}

std::optional<nlohmann::json> DesktopDatabase::select_investigation_brief_option(
    const std::string& task_id, const std::string& brief_id,
    const std::string& option_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) return std::nullopt;
  Statement read(connection_, R"SQL(
      SELECT id, task_id, generated_at, status, finding_ids, options, selected_option_id, plan_id
        FROM investigation_decision_briefs
       WHERE id = ?1
    )SQL");
  bind_text(connection_, read.get(), 1, brief_id);
  const auto result = sqlite3_step(read.get());
  if (result == SQLITE_DONE) return std::nullopt;
  if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
  auto brief = decode_investigation_brief(read.get());
  if (brief.value("taskId", std::string()) != task_id) {
    throw std::invalid_argument("decision brief does not belong to the current investigation task");
  }
  if (brief.value("status", std::string()) == "dismissed") {
    throw std::invalid_argument("dismissed decision briefs cannot be selected");
  }
  auto options = brief.value("options", nlohmann::json::array());
  if (!options.is_array()) throw std::runtime_error("stored decision brief options are invalid");
  bool found = false;
  for (auto& option : options) {
    if (!option.is_object()) continue;
    const bool selected = option.value("id", std::string()) == option_id;
    if (selected) found = true;
    option["status"] = selected ? "selected" : "rejected";
  }
  if (!found) {
    throw std::invalid_argument("decision option was not found in the selected brief");
  }
  Statement update(connection_, R"SQL(
      UPDATE investigation_decision_briefs
         SET status = ?3, selected_option_id = ?2, options = ?5
       WHERE id = ?1 AND task_id = ?4
    )SQL");
  bind_text(connection_, update.get(), 1, brief_id);
  bind_text(connection_, update.get(), 2, option_id);
  bind_text(connection_, update.get(), 3, "selected");
  bind_text(connection_, update.get(), 4, task_id);
  bind_text(connection_, update.get(), 5, options.dump());
  if (sqlite3_step(update.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  return get_investigation_brief(brief_id);
}

std::optional<nlohmann::json> DesktopDatabase::investigation_retention_preview(
    const std::string& task_id, const std::string& cutoff_at, int limit) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) return std::nullopt;
  if (!iso8601_utc_text(cutoff_at)) {
    throw std::invalid_argument("retention cutoff must be a UTC ISO-8601 timestamp");
  }
  if (limit < 1 || limit > 128) {
    throw std::invalid_argument("retention preview limit must be between 1 and 128");
  }
  const auto status = investigation_task_status(connection_, task_id);
  if (!status.has_value()) return std::nullopt;
  if (!terminal_task_status(*status)) {
    throw std::invalid_argument("retention is available only for terminal investigation tasks");
  }
  const auto protected_ids = collect_protected_evidence(connection_, task_id);
  std::vector<nlohmann::json> candidates;
  std::int64_t candidate_bytes = 0;
  std::int64_t protected_count = 0;
  const auto collect = [&](const char* sql, const char* kind, const char* reason,
                           bool can_be_protected, bool count_protected) {
    Statement statement(connection_, sql);
    bind_text(connection_, statement.get(), 1, task_id);
    bind_text(connection_, statement.get(), 2, cutoff_at);
    for (;;) {
      const auto result = sqlite3_step(statement.get());
      if (result == SQLITE_DONE) break;
      if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
      const auto id = std::string(column_text(statement.get(), 0));
      if (can_be_protected && protected_ids.find(id) != protected_ids.end()) {
        if (count_protected) ++protected_count;
        continue;
      }
      const auto bytes = sqlite3_column_int64(statement.get(), 3);
      candidate_bytes += bytes;
      candidates.push_back({{"id", id},
                            {"taskId", column_text(statement.get(), 1)},
                            {"kind", kind},
                            {"createdAt", column_text(statement.get(), 2)},
                            {"bytes", bytes},
                            {"reason", reason}});
    }
  };
  collect(R"SQL(
      SELECT id, task_id, collected_at, length(CAST(content AS BLOB))
        FROM investigation_evidence
       WHERE task_id = ?1 AND collected_at < ?2
    )SQL",
          "evidence", "unreferenced_evidence", true, true);
  collect(R"SQL(
      SELECT id, task_id, updated_at, length(CAST(content AS BLOB))
        FROM investigation_artifacts
       WHERE task_id = ?1 AND status = 'superseded' AND updated_at < ?2
    )SQL",
          "artifact", "superseded_artifact", false, false);
  std::sort(candidates.begin(), candidates.end(),
            [](const nlohmann::json& left, const nlohmann::json& right) {
              const auto left_time = left.value("createdAt", std::string());
              const auto right_time = right.value("createdAt", std::string());
              if (left_time != right_time) return left_time < right_time;
              const auto left_kind = left.value("kind", std::string());
              const auto right_kind = right.value("kind", std::string());
              if (left_kind != right_kind) return left_kind < right_kind;
              return left.value("id", std::string()) < right.value("id", std::string());
            });
  const auto total_candidates = static_cast<std::int64_t>(candidates.size());
  nlohmann::json bounded = nlohmann::json::array();
  for (std::int64_t index = 0; index < total_candidates && index < limit; ++index) {
    bounded.push_back(candidates[static_cast<size_t>(index)]);
  }
  return nlohmann::json{{"taskId", task_id},
                        {"cutoffAt", cutoff_at},
                        {"candidates", std::move(bounded)},
                        {"protectedCount", protected_count},
                        {"candidateBytes", candidate_bytes},
                        {"truncated", total_candidates > limit}};
}

std::optional<nlohmann::json> DesktopDatabase::investigation_retention_prune(
    const std::string& task_id, const std::string& cutoff_at, const nlohmann::json& items,
    const std::string& audit_id, const std::string& now) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) return std::nullopt;
  if (!iso8601_utc_text(cutoff_at)) {
    throw std::invalid_argument("retention cutoff must be a UTC ISO-8601 timestamp");
  }
  if (!items.is_array() || items.empty() || items.size() > 128) {
    throw std::invalid_argument("retention prune requires between 1 and 128 items");
  }
  const auto status = investigation_task_status(connection_, task_id);
  if (!status.has_value()) return std::nullopt;
  if (!terminal_task_status(*status)) {
    throw std::invalid_argument("retention is available only for terminal investigation tasks");
  }
  Transaction transaction(connection_);
  const auto protected_ids = collect_protected_evidence(connection_, task_id);
  nlohmann::json deleted = nlohmann::json::array();
  nlohmann::json skipped = nlohmann::json::array();
  std::set<std::string> seen;
  for (const auto& item : items) {
    if (!item.is_object()) {
      throw std::invalid_argument("retention prune items must be objects");
    }
    const auto id = item.value("id", std::string());
    const auto kind = item.value("kind", std::string());
    if (kind != "evidence" && kind != "artifact") {
      throw std::invalid_argument("retention item kind must be evidence or artifact");
    }
    if (id.empty()) throw std::invalid_argument("retention item id is required");
    if (!seen.insert(kind + "\x1f" + id).second) {
      throw std::invalid_argument("retention prune items must be unique");
    }
    if (kind == "evidence") {
      Statement read(connection_, R"SQL(
          SELECT id, task_id, collected_at, length(CAST(content AS BLOB))
            FROM investigation_evidence
           WHERE id = ?1 AND task_id = ?2
        )SQL");
      bind_text(connection_, read.get(), 1, id);
      bind_text(connection_, read.get(), 2, task_id);
      const auto result = sqlite3_step(read.get());
      if (result == SQLITE_DONE) {
        skipped.push_back({{"id", id}, {"kind", kind}, {"reason", "not_found"}});
        continue;
      }
      if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
      const auto owner = std::string(column_text(read.get(), 1));
      const auto created_at = std::string(column_text(read.get(), 2));
      const auto bytes = sqlite3_column_int64(read.get(), 3);
      if (created_at >= cutoff_at) {
        skipped.push_back({{"id", id}, {"kind", kind}, {"reason", "newer_than_cutoff"}});
        continue;
      }
      if (protected_ids.find(id) != protected_ids.end()) {
        skipped.push_back(
            {{"id", id}, {"kind", kind}, {"reason", "referenced_by_task_history"}});
        continue;
      }
      Statement remove(connection_, R"SQL(
          DELETE FROM investigation_evidence
           WHERE id = ?1 AND task_id = ?2 AND collected_at < ?3
        )SQL");
      bind_text(connection_, remove.get(), 1, id);
      bind_text(connection_, remove.get(), 2, task_id);
      bind_text(connection_, remove.get(), 3, cutoff_at);
      if (sqlite3_step(remove.get()) != SQLITE_DONE) {
        throw std::runtime_error(sqlite3_errmsg(connection_));
      }
      if (sqlite3_changes(connection_) == 1) {
        deleted.push_back({{"id", id},
                           {"taskId", owner},
                           {"kind", kind},
                           {"createdAt", created_at},
                           {"bytes", bytes},
                           {"reason", "unreferenced_evidence"}});
      } else {
        skipped.push_back({{"id", id}, {"kind", kind}, {"reason", "changed_during_prune"}});
      }
    } else {
      Statement read(connection_, R"SQL(
          SELECT id, task_id, updated_at, length(CAST(content AS BLOB)), status
            FROM investigation_artifacts
           WHERE id = ?1 AND task_id = ?2
        )SQL");
      bind_text(connection_, read.get(), 1, id);
      bind_text(connection_, read.get(), 2, task_id);
      const auto result = sqlite3_step(read.get());
      if (result == SQLITE_DONE) {
        skipped.push_back({{"id", id}, {"kind", kind}, {"reason", "not_found"}});
        continue;
      }
      if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
      const auto owner = std::string(column_text(read.get(), 1));
      const auto updated_at = std::string(column_text(read.get(), 2));
      const auto bytes = sqlite3_column_int64(read.get(), 3);
      const auto artifact_status = std::string(column_text(read.get(), 4));
      if (artifact_status != "superseded") {
        skipped.push_back({{"id", id}, {"kind", kind}, {"reason", "artifact_not_superseded"}});
        continue;
      }
      if (updated_at >= cutoff_at) {
        skipped.push_back({{"id", id}, {"kind", kind}, {"reason", "newer_than_cutoff"}});
        continue;
      }
      Statement remove(connection_, R"SQL(
          DELETE FROM investigation_artifacts
           WHERE id = ?1 AND task_id = ?2 AND status = 'superseded' AND updated_at < ?3
        )SQL");
      bind_text(connection_, remove.get(), 1, id);
      bind_text(connection_, remove.get(), 2, task_id);
      bind_text(connection_, remove.get(), 3, cutoff_at);
      if (sqlite3_step(remove.get()) != SQLITE_DONE) {
        throw std::runtime_error(sqlite3_errmsg(connection_));
      }
      if (sqlite3_changes(connection_) == 1) {
        deleted.push_back({{"id", id},
                           {"taskId", owner},
                           {"kind", kind},
                           {"createdAt", updated_at},
                           {"bytes", bytes},
                           {"reason", "superseded_artifact"}});
      } else {
        skipped.push_back({{"id", id}, {"kind", kind}, {"reason", "changed_during_prune"}});
      }
    }
  }
  std::int64_t deleted_evidence = 0;
  std::int64_t deleted_artifacts = 0;
  for (const auto& entry : deleted) {
    if (entry.value("kind", std::string()) == "evidence") {
      ++deleted_evidence;
    } else {
      ++deleted_artifacts;
    }
  }
  const std::string description =
      "retention prune task=" + task_id + " cutoff=" + cutoff_at +
      " deleted_evidence=" + std::to_string(deleted_evidence) +
      " deleted_artifacts=" + std::to_string(deleted_artifacts) +
      " skipped=" + std::to_string(skipped.size());
  Statement activity(connection_, R"SQL(
      INSERT INTO activities (
        id, server_id, workspace_id, type, title, description, source, actor,
        reason, outcome, trace_id, created_at
      )
      SELECT ?1, server_id, workspace_id, 'agent_action',
             '清理排查任务历史数据', ?2, 'user', 'user',
             'explicit_retention_prune', 'success', ?1, ?3
      FROM investigation_tasks WHERE id = ?4
    )SQL");
  bind_text(connection_, activity.get(), 1, audit_id);
  bind_text(connection_, activity.get(), 2, description);
  bind_text(connection_, activity.get(), 3, now);
  bind_text(connection_, activity.get(), 4, task_id);
  if (sqlite3_step(activity.get()) != SQLITE_DONE) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  transaction.commit();
  return nlohmann::json{{"taskId", task_id},
                        {"cutoffAt", cutoff_at},
                        {"deleted", std::move(deleted)},
                        {"skipped", std::move(skipped)}};
}

nlohmann::json DesktopDatabase::list_chat_sessions(
    const std::optional<std::string>& query, const std::optional<bool>& archived,
    int limit, int offset) const {
  std::lock_guard lock(mutex_);
  if (limit < 1 || limit > 100 || offset < 0 || offset > 10'000) {
    throw std::invalid_argument("Chat session paging is out of range");
  }
  if (connection_ == nullptr) {
    return {{"sessions", nlohmann::json::array()},
            {"counts", {{"active", 0}, {"archived", 0}}}};
  }

  std::string pattern = "%";
  pattern += escape_like(query.value_or(""));
  pattern += "%";
  std::string archive_clause;
  if (archived == true) archive_clause = "AND s.archived_at IS NOT NULL";
  else if (archived == false) archive_clause = "AND s.archived_at IS NULL";

  const std::string list_sql = std::string(kChatSessionSelect) + R"SQL(
     WHERE (s.title LIKE ?1 ESCAPE '\'
        OR EXISTS (
          SELECT 1 FROM chat_messages search_messages
           WHERE search_messages.session_id = s.id
             AND search_messages.content LIKE ?1 ESCAPE '\'
        ))
  )SQL" + archive_clause + R"SQL(
     GROUP BY s.id
     ORDER BY s.updated_at DESC, s.id DESC
     LIMIT ?2 OFFSET ?3
  )SQL";
  Statement rows(connection_, list_sql.c_str());
  bind_text(connection_, rows.get(), 1, pattern);
  if (sqlite3_bind_int(rows.get(), 2, limit) != SQLITE_OK ||
      sqlite3_bind_int(rows.get(), 3, offset) != SQLITE_OK) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  nlohmann::json sessions = nlohmann::json::array();
  for (;;) {
    const auto result = sqlite3_step(rows.get());
    if (result == SQLITE_DONE) break;
    if (result != SQLITE_ROW) throw std::runtime_error(sqlite3_errmsg(connection_));
    sessions.push_back(chat_session_from_row(connection_, rows.get()));
  }

  Statement count_query(connection_, R"SQL(
    SELECT COALESCE(SUM(CASE WHEN s.archived_at IS NULL THEN 1 ELSE 0 END), 0),
           COALESCE(SUM(CASE WHEN s.archived_at IS NOT NULL THEN 1 ELSE 0 END), 0)
      FROM chat_sessions s
     WHERE (s.title LIKE ?1 ESCAPE '\'
        OR EXISTS (
          SELECT 1 FROM chat_messages search_messages
           WHERE search_messages.session_id = s.id
             AND search_messages.content LIKE ?1 ESCAPE '\'
        ))
  )SQL");
  bind_text(connection_, count_query.get(), 1, pattern);
  if (sqlite3_step(count_query.get()) != SQLITE_ROW) {
    throw std::runtime_error(sqlite3_errmsg(connection_));
  }
  const auto active = sqlite3_column_int64(count_query.get(), 0);
  const auto archived_count = sqlite3_column_int64(count_query.get(), 1);
  if (active < 0 || archived_count < 0 ||
      static_cast<std::uint64_t>(active) > std::numeric_limits<std::uint32_t>::max() ||
      static_cast<std::uint64_t>(archived_count) > std::numeric_limits<std::uint32_t>::max()) {
    throw std::runtime_error("Stored chat session count is out of range");
  }
  return {{"sessions", std::move(sessions)},
          {"counts", {{"active", static_cast<std::uint32_t>(active)},
                       {"archived", static_cast<std::uint32_t>(archived_count)}}}};
}

nlohmann::json DesktopDatabase::get_chat_session(const std::string& session_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local chat database is unavailable");
  return query_chat_session(connection_, session_id);
}

nlohmann::json DesktopDatabase::create_chat_session(
    const std::string& id, const std::optional<std::string>& workspace_id,
    const std::optional<std::string>& server_id, const std::string& title,
    const std::string& timestamp) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local chat database is unavailable");
  Transaction transaction(connection_);
  Statement insert(connection_, R"SQL(
    INSERT INTO chat_sessions (
      id, workspace_id, server_id, title, created_at, updated_at, archived_at
    ) VALUES (?1, ?2, ?3, ?4, ?5, ?5, NULL)
  )SQL");
  bind_text(connection_, insert.get(), 1, id);
  bind_optional_text(connection_, insert.get(), 2, workspace_id);
  bind_optional_text(connection_, insert.get(), 3, server_id);
  bind_text(connection_, insert.get(), 4, title);
  bind_text(connection_, insert.get(), 5, timestamp);
  if (sqlite3_step(insert.get()) != SQLITE_DONE) throw std::runtime_error(sqlite3_errmsg(connection_));
  transaction.commit();
  const auto detail = query_chat_session(connection_, id);
  return {{"session", detail.at("session")}};
}

nlohmann::json DesktopDatabase::append_chat_message(
    const std::string& id, const std::string& session_id, const std::string& role,
    const std::string& content, const std::optional<nlohmann::json>& parts,
    const std::optional<std::string>& trace_id, const std::string& timestamp) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local chat database is unavailable");
  if (!is_one_of(role, {"user", "assistant", "tool", "system"})) {
    throw std::invalid_argument("Chat message role is invalid");
  }
  std::optional<std::string> encoded_parts;
  if (parts.has_value()) {
    if (!parts->is_array() || parts->empty() || parts->size() > 25) {
      throw std::invalid_argument("Chat prompt parts are out of range");
    }
    encoded_parts = parts->dump();
    if (encoded_parts->size() > 24 * 1024 * 1024) {
      throw std::invalid_argument("Chat prompt parts exceed the frame limit");
    }
  }

  Transaction transaction(connection_);
  Statement insert(connection_, R"SQL(
    INSERT INTO chat_messages (
      id, session_id, role, content, parts_json, trace_id, created_at
    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
  )SQL");
  bind_text(connection_, insert.get(), 1, id);
  bind_text(connection_, insert.get(), 2, session_id);
  bind_text(connection_, insert.get(), 3, role);
  bind_text(connection_, insert.get(), 4, content);
  bind_optional_text(connection_, insert.get(), 5, encoded_parts);
  bind_optional_text(connection_, insert.get(), 6, trace_id);
  bind_text(connection_, insert.get(), 7, timestamp);
  if (sqlite3_step(insert.get()) != SQLITE_DONE) throw std::runtime_error(sqlite3_errmsg(connection_));

  Statement update(connection_, "UPDATE chat_sessions SET updated_at = ?2 WHERE id = ?1");
  bind_text(connection_, update.get(), 1, session_id);
  bind_text(connection_, update.get(), 2, timestamp);
  if (sqlite3_step(update.get()) != SQLITE_DONE) throw std::runtime_error(sqlite3_errmsg(connection_));
  if (sqlite3_changes(connection_) == 0) throw std::runtime_error("Chat session was not found");
  transaction.commit();

  nlohmann::json message = {
      {"id", id}, {"sessionId", session_id}, {"role", role}, {"content", content},
      {"createdAt", timestamp},
  };
  if (parts.has_value()) message["parts"] = *parts;
  if (trace_id.has_value()) message["traceId"] = *trace_id;
  return {{"message", std::move(message)}};
}

nlohmann::json DesktopDatabase::set_chat_session_archived(
    const std::string& session_id,
    const std::optional<std::string>& archived_at) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local chat database is unavailable");
  Transaction transaction(connection_);
  Statement update(connection_, "UPDATE chat_sessions SET archived_at = ?2 WHERE id = ?1");
  bind_text(connection_, update.get(), 1, session_id);
  bind_optional_text(connection_, update.get(), 2, archived_at);
  if (sqlite3_step(update.get()) != SQLITE_DONE) throw std::runtime_error(sqlite3_errmsg(connection_));
  if (sqlite3_changes(connection_) == 0) throw std::runtime_error("Chat session was not found");
  transaction.commit();
  return {{"session", query_chat_session(connection_, session_id).at("session")}};
}

nlohmann::json DesktopDatabase::rename_chat_session(
    const std::string& session_id, const std::string& title) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local chat database is unavailable");
  Transaction transaction(connection_);
  Statement update(connection_, "UPDATE chat_sessions SET title = ?2 WHERE id = ?1");
  bind_text(connection_, update.get(), 1, session_id);
  bind_text(connection_, update.get(), 2, title);
  if (sqlite3_step(update.get()) != SQLITE_DONE) throw std::runtime_error(sqlite3_errmsg(connection_));
  if (sqlite3_changes(connection_) == 0) throw std::runtime_error("Chat session was not found");
  transaction.commit();
  return {{"session", query_chat_session(connection_, session_id).at("session")}};
}

nlohmann::json DesktopDatabase::delete_chat_session(const std::string& session_id) const {
  std::lock_guard lock(mutex_);
  if (connection_ == nullptr) throw std::runtime_error("The local chat database is unavailable");
  Transaction transaction(connection_);
  Statement remove(connection_, "DELETE FROM chat_sessions WHERE id = ?1");
  bind_text(connection_, remove.get(), 1, session_id);
  if (sqlite3_step(remove.get()) != SQLITE_DONE) throw std::runtime_error(sqlite3_errmsg(connection_));
  if (sqlite3_changes(connection_) == 0) throw std::runtime_error("Chat session was not found");
  transaction.commit();
  return {{"deleted", true}};
}

std::filesystem::path default_data_directory() {
  if (const auto override = environment_path("YUKINAL_DATA_DIR"); !override.empty()) {
    return override;
  }
#ifdef _WIN32
  auto base = environment_path("APPDATA");
  if (base.empty()) base = environment_path("USERPROFILE") / "AppData" / "Roaming";
  return base / "dev.yukinal.workspace";
#elif defined(__APPLE__)
  return environment_path("HOME") / "Library" / "Application Support" / "dev.yukinal.workspace";
#else
  auto base = environment_path("XDG_DATA_HOME");
  if (base.empty()) base = environment_path("HOME") / ".local" / "share";
  return base / "dev.yukinal.workspace";
#endif
}

}  // namespace yukinal::desktop_host

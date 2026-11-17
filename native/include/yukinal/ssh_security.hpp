#pragma once

#include <algorithm>
#include <atomic>
#include <chrono>
#include <memory>
#include <stdexcept>
#include <string>
#include <string_view>
#include <utility>

namespace yukinal::ssh_service {

// The only quoting primitive for values embedded in POSIX remote commands.
// A quoted value remains one shell word even with quotes, newlines or `...`.
inline std::string shell_quote(std::string_view value) {
  if (value.find('\0') != std::string_view::npos) throw std::invalid_argument("NUL in shell argument");
  std::string result = "'";
  for (const char ch : value) result += ch == '\'' ? "'\\''" : std::string(1, ch);
  return result + "'";
}

inline bool safe_sftp_path(std::string_view path) {
  if (path.empty() || path.size() > 4096 || (path != "." && path.front() != '/')) return false;
  if (std::any_of(path.begin(), path.end(), [](unsigned char c) { return c < 32 || c == 127; })) return false;
  for (std::size_t start = 0; start <= path.size();) {
    const auto end = path.find('/', start);
    if (path.substr(start, end == path.npos ? path.npos : end - start) == "..") return false;
    if (end == path.npos) break;
    start = end + 1;
  }
  return true;
}

inline bool path_within_root(std::string_view path, std::string_view root) {
  return root == "/" || path == root ||
         (path.size() > root.size() && path.starts_with(root) && path[root.size()] == '/');
}

// Stateful across network frames: an OSC/DCS/CSI split across reads must never
// be reconstructed by xterm. Only numeric SGR parameters are emitted.
class SgrOutputFilter {
 public:
  std::string consume(std::string_view input) {
    std::string output;
    for (unsigned char ch : input) {
      // Decode enough UTF-8 to recognize encoded C1 controls. Buffer complete
      // characters so a split C2 9B cannot become a CSI after xterm decodes it.
      if (utf8_remaining_) {
        if (ch >= 0x80 && ch <= 0xbf) {
          utf8_pending_ += static_cast<char>(ch);
          if (--utf8_remaining_ != 0) continue;
          if (utf8_pending_.size() == 2 && static_cast<unsigned char>(utf8_pending_[0]) == 0xc2 && ch <= 0x9f) {
            utf8_pending_.clear(); // fall through as a C1 control
          } else {
            if (state_ == State::text) output += utf8_pending_;
            utf8_pending_.clear(); continue;
          }
        } else { utf8_pending_.clear(); utf8_remaining_ = 0; }
      } else if (ch >= 0xc2 && ch <= 0xf4) {
        utf8_remaining_ = ch <= 0xdf ? 1 : ch <= 0xef ? 2 : 3;
        utf8_pending_.assign(1, static_cast<char>(ch)); continue;
      }
      switch (state_) {
        case State::text:
          if (ch == 0x1b) state_ = State::escape;
          else if (ch == 0x9b) { state_ = State::csi; pending_ = "\x1b["; }
          else if (ch == 0x9d || ch == 0x90 || ch == 0x98 || ch == 0x9e || ch == 0x9f) { state_ = State::string; string_osc_ = ch == 0x9d; }
          else if (ch == '\n' || ch == '\r' || ch == '\t' || (ch >= 32 && ch != 127 && (ch < 128 || ch >= 160))) output += static_cast<char>(ch);
          break;
        case State::escape:
          if (ch == '[') { state_ = State::csi; pending_ = "\x1b["; }
          else if (ch == ']' || ch == 'P' || ch == 'X' || ch == '^' || ch == '_') { state_ = State::string; string_osc_ = ch == ']'; }
          else if (ch >= 0x20 && ch <= 0x2f) state_ = State::escape_intermediate;
          else state_ = State::text;
          break;
        case State::escape_intermediate:
          if (ch >= 0x30 && ch <= 0x7e) state_ = State::text;
          break;
        case State::csi:
          if (ch == 'm') {
            if (!pending_.empty()) output += pending_ + 'm';
            pending_.clear(); state_ = State::text;
          } else if (ch >= 0x40 && ch <= 0x7e) { pending_.clear(); state_ = State::text; }
          else if (!pending_.empty() && ((ch >= '0' && ch <= '9') || ch == ';' || ch == ':') && pending_.size() < 128) pending_ += static_cast<char>(ch);
          else pending_.clear();
          break;
        case State::string:
          if ((ch == 7 && string_osc_) || ch == 0x9c) state_ = State::text;
          else if (ch == 0x1b) state_ = State::string_escape;
          break;
        case State::string_escape:
          if (ch == '\\' || (ch == 7 && string_osc_) || ch == 0x9c) state_ = State::text;
          else if (ch != 0x1b) state_ = State::string;
          break;
      }
    }
    return output;
  }
 private:
  enum class State { text, escape, escape_intermediate, csi, string, string_escape };
  State state_ = State::text;
  std::string pending_;
  unsigned int utf8_remaining_ = 0;
  std::string utf8_pending_;
  bool string_osc_ = false;
};

struct ExecutionLease {
  std::string id;
  std::atomic_bool cancelled{false};
  std::chrono::system_clock::time_point expires = std::chrono::system_clock::time_point::max();
  bool valid() const { return !cancelled.load() && std::chrono::system_clock::now() < expires; }
};

inline thread_local std::shared_ptr<ExecutionLease> current_execution_lease;
inline thread_local std::string_view current_audit_source = "system";
class ScopedAuditSource {
 public:
  explicit ScopedAuditSource(std::string_view source) : previous_(current_audit_source) { current_audit_source = source; }
  ~ScopedAuditSource() { current_audit_source = previous_; }
 private:
  std::string_view previous_;
};
class ScopedExecutionLease {
 public:
  explicit ScopedExecutionLease(std::shared_ptr<ExecutionLease> lease)
      : previous_(std::move(current_execution_lease)) { current_execution_lease = std::move(lease); }
  ~ScopedExecutionLease() { current_execution_lease = std::move(previous_); }
 private:
  std::shared_ptr<ExecutionLease> previous_;
};

inline bool execution_allowed() { return !current_execution_lease || current_execution_lease->valid(); }

}  // namespace yukinal::ssh_service

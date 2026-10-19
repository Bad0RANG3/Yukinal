#pragma once

#include <cstddef>
#include <string>

namespace yukinal::desktop_host {

// Private to the desktop host implementation: both the RPC validation layer
// (`ipc_router.cpp`) and the MCP transport (`desktop_mcp.cpp`) must agree on
// what is safe, and the host test target includes `desktop_host/src`.
//
// Mirrors Rust's `char::is_control`: C0, DEL and the C1 range (U+0080..U+009F,
// which UTF-8 encodes as 0xC2 followed by 0x80..0x9F).
//
// This is the one predicate for HTTP header material. httplib's
// `write_headers` copies header names and values into the request verbatim, so
// a value containing CR, LF or NUL is a header-injection vector, and the C1
// controls are rejected too because intermediaries may normalize them into
// separators. Header names and static auth header *values* must both be
// checked with this function, at write time and again at load time.
inline bool has_header_control_characters(const std::string& value) {
  for (std::size_t index = 0; index < value.size(); ++index) {
    const auto byte = static_cast<unsigned char>(value[index]);
    if (byte < 0x20 || byte == 0x7f) return true;
    if (byte == 0xc2 && index + 1 < value.size()) {
      const auto next = static_cast<unsigned char>(value[index + 1]);
      if (next >= 0x80 && next <= 0x9f) return true;
    }
  }
  return false;
}

}  // namespace yukinal::desktop_host

#pragma once

#include <string>

namespace yukinal::desktop_host {

// Lowercase-hex SHA-256 over the raw bytes of `input`.
//
// This is the single hashing entry point the desktop host uses for content
// revisions (`filesystem.read`/`edit` revisions, evidence content hashes) and for
// the host-derived filesystem backup path. It lives behind a narrow declaration so
// the implementation (a dependency-free SHA-256) stays private to one translation
// unit while every caller agrees on the exact bytes and encoding.
[[nodiscard]] std::string content_revision_hex(const std::string& input);

}  // namespace yukinal::desktop_host

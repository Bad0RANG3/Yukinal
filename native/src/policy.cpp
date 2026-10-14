#include "yukinal/policy.hpp"

#include <algorithm>
#include <cstdint>
#include <limits>
#include <regex>
#include <set>
#include <string_view>

namespace yukinal::ssh_service {
namespace {

void set_error(ApiError* error, int status, std::string code, std::string message) {
  if (!error) return;
  error->http_status = status;
  error->code = std::move(code);
  error->message = std::move(message);
}

bool has_only_keys(const nlohmann::json& value, const std::set<std::string>& allowed) {
  for (auto it = value.begin(); it != value.end(); ++it) {
    if (!allowed.contains(it.key())) return false;
  }
  return true;
}

bool valid_systemd_unit(const std::string& value) {
  const auto is_ascii_alphanumeric = [](unsigned char character) {
    return (character >= 'A' && character <= 'Z') ||
           (character >= 'a' && character <= 'z') ||
           (character >= '0' && character <= '9');
  };
  if (value.size() < 8 || value.size() > 128 ||
      value.compare(value.size() - 8, 8, ".service") != 0 ||
      !is_ascii_alphanumeric(static_cast<unsigned char>(value.front()))) {
    return false;
  }
  return std::all_of(value.begin(), value.end(), [&](unsigned char character) {
    return is_ascii_alphanumeric(character) || character == '_' || character == '.' ||
           character == '@' || character == ':' || character == '-';
  });
}

int base64_value(char character) {
  if (character >= 'A' && character <= 'Z') return character - 'A';
  if (character >= 'a' && character <= 'z') return character - 'a' + 26;
  if (character >= '0' && character <= '9') return character - '0' + 52;
  if (character == '+') return 62;
  if (character == '/') return 63;
  return -1;
}

bool decode_base64(std::string_view encoded, std::size_t maximum,
                   std::vector<unsigned char>* decoded) {
  if (encoded.size() % 4 != 0) return false;
  const std::size_t padding = encoded.ends_with("==") ? 2 : encoded.ends_with("=") ? 1 : 0;
  if (encoded.size() > ((maximum + 2) / 3) * 4) return false;
  for (std::size_t index = 0; index < encoded.size(); ++index) {
    const bool is_padding = encoded[index] == '=';
    if ((is_padding && (padding == 0 || index < encoded.size() - padding)) ||
        (!is_padding && base64_value(encoded[index]) < 0)) {
      return false;
    }
  }
  if (padding == 2 && encoded.size() >= 4 &&
      (base64_value(encoded[encoded.size() - 3]) & 0x0f) != 0) {
    return false;
  }
  if (padding == 1 && encoded.size() >= 4 &&
      (base64_value(encoded[encoded.size() - 2]) & 0x03) != 0) {
    return false;
  }

  const std::size_t output_size = (encoded.size() / 4) * 3 - padding;
  if (output_size > maximum) return false;
  if (!decoded) return false;
  decoded->clear();
  decoded->reserve(output_size);
  for (std::size_t index = 0; index < encoded.size(); index += 4) {
    const unsigned int first = static_cast<unsigned int>(base64_value(encoded[index]));
    const unsigned int second = static_cast<unsigned int>(base64_value(encoded[index + 1]));
    const unsigned int third = encoded[index + 2] == '='
                                   ? 0U
                                   : static_cast<unsigned int>(base64_value(encoded[index + 2]));
    const unsigned int fourth = encoded[index + 3] == '='
                                    ? 0U
                                    : static_cast<unsigned int>(base64_value(encoded[index + 3]));
    const unsigned int block = (first << 18) | (second << 12) | (third << 6) | fourth;
    decoded->push_back(static_cast<unsigned char>((block >> 16) & 0xff));
    if (encoded[index + 2] != '=') decoded->push_back(static_cast<unsigned char>((block >> 8) & 0xff));
    if (encoded[index + 3] != '=') decoded->push_back(static_cast<unsigned char>(block & 0xff));
  }
  return decoded->size() == output_size;
}

}  // namespace

bool valid_target_id(const std::string& value) {
  static const std::regex pattern("^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$");
  return std::regex_match(value, pattern);
}

bool valid_service_name(const std::string& value) {
  static const std::regex pattern("^[A-Za-z0-9][A-Za-z0-9_.@:-]{0,127}$");
  return std::regex_match(value, pattern);
}

bool valid_container_ref(const std::string& value) {
  if (value.empty() || value.size() > 128) return false;
  const auto is_ascii_alphanumeric = [](unsigned char character) {
    return (character >= 'A' && character <= 'Z') ||
           (character >= 'a' && character <= 'z') ||
           (character >= '0' && character <= '9');
  };
  if (!is_ascii_alphanumeric(static_cast<unsigned char>(value.front()))) return false;
  return std::all_of(value.begin() + 1, value.end(), [&](unsigned char character) {
    return is_ascii_alphanumeric(character) || character == '_' || character == '.' ||
           character == '-';
  });
}

bool valid_package_manager(const std::string& value) {
  return value == "apt" || value == "dnf";
}

bool valid_package_ref(const std::string& value) {
  if (value.empty() || value.size() > 128) return false;
  const auto is_ascii_alphanumeric = [](unsigned char character) {
    return (character >= 'A' && character <= 'Z') ||
           (character >= 'a' && character <= 'z') ||
           (character >= '0' && character <= '9');
  };
  if (!is_ascii_alphanumeric(static_cast<unsigned char>(value.front()))) return false;
  return std::all_of(value.begin() + 1, value.end(), [&](unsigned char character) {
    return is_ascii_alphanumeric(character) || character == '+' || character == '.' ||
           character == '_' || character == ':' || character == '-';
  });
}

bool valid_sftp_path(const std::string& value) {
  return !value.empty() && value.size() <= 4096 && value.find('\0') == std::string::npos;
}

bool valid_snapshot_nonce(const std::string& value) {
  return value.size() == 32 && std::all_of(value.begin(), value.end(), [](unsigned char byte) {
    return (byte >= '0' && byte <= '9') || (byte >= 'a' && byte <= 'f');
  });
}

bool parse_exec_request(const nlohmann::json& value, ExecRequest* request, ApiError* error) {
  if (!value.is_object() || !has_only_keys(value, {"target_id", "operation", "parameters", "timeout_seconds"})) {
    set_error(error, 400, "invalid_request", "Request must contain only the documented fields");
    return false;
  }
  const auto target_it = value.find("target_id");
  const auto operation_it = value.find("operation");
  const auto parameters_it = value.find("parameters");
  const auto timeout_it = value.find("timeout_seconds");
  if (target_it == value.end() || !target_it->is_string() ||
      operation_it == value.end() || !operation_it->is_string() ||
      parameters_it == value.end() || !parameters_it->is_object() ||
      timeout_it == value.end() ||
      !(timeout_it->is_number_integer() || timeout_it->is_number_unsigned())) {
    set_error(error, 400, "invalid_request", "Request fields have invalid types or are missing");
    return false;
  }

  const std::string target_id = target_it->get<std::string>();
  if (!valid_target_id(target_id)) {
    set_error(error, 422, "invalid_target_id", "target_id has an invalid format");
    return false;
  }
  const std::string operation = operation_it->get<std::string>();
  std::string service;
  std::string unit;
  std::string path;
  std::string source_path;
  std::string destination_path;
  std::string nonce;
  std::string package_manager;
  std::string package;
  std::string container;
  bool include_all_containers = false;
  std::uint32_t tail = 120;
  std::vector<unsigned char> content;
  std::uint64_t expected_size = 0;
  std::optional<std::uint32_t> expected_modified;
  std::array<unsigned char, 32> expected_digest{};
  std::uint32_t max_bytes = 0;
  std::optional<std::uint32_t> since_seconds;
  if (operation == "connection_check") {
    if (!parameters_it->empty()) {
      set_error(error, 422, "invalid_parameters", "connection_check accepts no parameters");
      return false;
    }
  } else if (operation == "system_snapshot") {
    if (!has_only_keys(*parameters_it, {"nonce"})) {
      set_error(error, 422, "invalid_parameters", "system_snapshot accepts only a nonce parameter");
      return false;
    }
    const auto nonce_it = parameters_it->find("nonce");
    if (nonce_it == parameters_it->end() || !nonce_it->is_string() ||
        !valid_snapshot_nonce(nonce_it->get<std::string>())) {
      set_error(error, 422, "invalid_parameters", "system_snapshot nonce must be 32 lowercase hexadecimal characters");
      return false;
    }
    nonce = nonce_it->get<std::string>();
  } else if (operation == "docker_ps") {
    if (!has_only_keys(*parameters_it, {"all"})) {
      set_error(error, 422, "invalid_parameters", "docker_ps accepts only an all parameter");
      return false;
    }
    const auto all_it = parameters_it->find("all");
    if (all_it == parameters_it->end() || !all_it->is_boolean()) {
      set_error(error, 422, "invalid_parameters", "docker_ps requires a boolean all parameter");
      return false;
    }
    include_all_containers = all_it->get<bool>();
  } else if (operation == "docker_logs") {
    if (!has_only_keys(*parameters_it, {"container", "tail"})) {
      set_error(error, 422, "invalid_parameters", "docker_logs accepts only container and tail parameters");
      return false;
    }
    const auto container_it = parameters_it->find("container");
    const auto tail_it = parameters_it->find("tail");
    if (container_it == parameters_it->end() || !container_it->is_string() ||
        !valid_container_ref(container_it->get<std::string>()) ||
        tail_it == parameters_it->end()) {
      set_error(error, 422, "invalid_parameters", "docker_logs requires a safe container and bounded tail");
      return false;
    }
    std::uint64_t parsed_tail = 0;
    if (tail_it->is_number_unsigned()) {
      parsed_tail = tail_it->get<std::uint64_t>();
    } else if (tail_it->is_number_integer()) {
      const auto signed_tail = tail_it->get<std::int64_t>();
      if (signed_tail > 0) parsed_tail = static_cast<std::uint64_t>(signed_tail);
    }
    if (parsed_tail < 1 || parsed_tail > 500) {
      set_error(error, 422, "invalid_parameters", "docker_logs tail must be between 1 and 500");
      return false;
    }
    container = container_it->get<std::string>();
    tail = static_cast<std::uint32_t>(parsed_tail);
  } else if (operation == "docker_inspect") {
    if (!has_only_keys(*parameters_it, {"container"})) {
      set_error(error, 422, "invalid_parameters", "docker_inspect accepts only a container parameter");
      return false;
    }
    const auto container_it = parameters_it->find("container");
    if (container_it == parameters_it->end() || !container_it->is_string() ||
        !valid_container_ref(container_it->get<std::string>())) {
      set_error(error, 422, "invalid_parameters", "docker_inspect requires a safe container reference");
      return false;
    }
    container = container_it->get<std::string>();
  } else if (operation == "systemd_inspect") {
    if (!has_only_keys(*parameters_it, {"service"})) {
      set_error(error, 422, "invalid_parameters", "systemd_inspect accepts only a service parameter");
      return false;
    }
    const auto service_it = parameters_it->find("service");
    if (service_it == parameters_it->end() || !service_it->is_string()) {
      set_error(error, 422, "invalid_parameters", "systemd_inspect requires a .service unit name");
      return false;
    }
    unit = service_it->get<std::string>();
    if (!valid_service_name(unit) || !unit.ends_with(".service")) {
      set_error(error, 422, "invalid_parameters", "systemd_inspect requires a safe .service unit name");
      return false;
    }
  } else if (operation == "package_inspect") {
    if (!has_only_keys(*parameters_it, {"manager", "package"})) {
      set_error(error, 422, "invalid_parameters", "package_inspect accepts only manager and package parameters");
      return false;
    }
    const auto manager_it = parameters_it->find("manager");
    const auto package_it = parameters_it->find("package");
    if (manager_it == parameters_it->end() || !manager_it->is_string() ||
        package_it == parameters_it->end() || !package_it->is_string()) {
      set_error(error, 422, "invalid_parameters", "package_inspect requires a supported manager and safe package name");
      return false;
    }
    package_manager = manager_it->get<std::string>();
    package = package_it->get<std::string>();
    if (!valid_package_manager(package_manager) || !valid_package_ref(package)) {
      set_error(error, 422, "invalid_parameters", "package_inspect requires apt or dnf and a safe package name");
      return false;
    }
  } else if (operation == "service_status") {
    if (!has_only_keys(*parameters_it, {"service"})) {
      set_error(error, 422, "invalid_parameters", "service_status accepts only a service parameter");
      return false;
    }
    const auto service_it = parameters_it->find("service");
    if (service_it == parameters_it->end() || !service_it->is_string() ||
        !valid_service_name(service_it->get<std::string>())) {
      set_error(error, 422, "invalid_parameters", "service must be a valid systemd unit name");
      return false;
    }
    service = service_it->get<std::string>();
  } else if (operation == "host_key_probe") {
    if (!parameters_it->empty()) {
      set_error(error, 422, "invalid_parameters", "host_key_probe accepts no parameters");
      return false;
    }
  } else if (operation == "service_inventory") {
    if (!parameters_it->empty()) {
      set_error(error, 422, "invalid_parameters", "service_inventory accepts no parameters");
      return false;
    }
  } else if (operation == "server_logs") {
    if (!has_only_keys(*parameters_it, {"since_seconds", "unit"})) {
      set_error(error, 422, "invalid_parameters", "server_logs accepts only since_seconds and unit parameters");
      return false;
    }
    const auto since_it = parameters_it->find("since_seconds");
    if (since_it != parameters_it->end()) {
      std::uint64_t seconds = 0;
      if (since_it->is_number_unsigned()) {
        seconds = since_it->get<std::uint64_t>();
      } else if (since_it->is_number_integer()) {
        const auto signed_seconds = since_it->get<std::int64_t>();
        if (signed_seconds > 0) seconds = static_cast<std::uint64_t>(signed_seconds);
      }
      if (seconds < 1 || seconds > 604800) {
        set_error(error, 422, "invalid_parameters", "since_seconds must be between 1 and 604800");
        return false;
      }
      since_seconds = static_cast<std::uint32_t>(seconds);
    }
    const auto unit_it = parameters_it->find("unit");
    if (unit_it != parameters_it->end()) {
      if (!unit_it->is_string() || !valid_systemd_unit(unit_it->get<std::string>())) {
        set_error(error, 422, "invalid_parameters", "unit must be a bounded .service reference");
        return false;
      }
      unit = unit_it->get<std::string>();
    }
  } else if (operation == "sftp_list" || operation == "sftp_stat" ||
             operation == "sftp_lstat_optional") {
    if (!has_only_keys(*parameters_it, {"path"})) {
      set_error(error, 422, "invalid_parameters", "SFTP metadata operations accept only a path parameter");
      return false;
    }
    const auto path_it = parameters_it->find("path");
    if (path_it == parameters_it->end() || !path_it->is_string() ||
        !valid_sftp_path(path_it->get<std::string>())) {
      set_error(error, 422, "invalid_parameters", "path must be a non-empty SFTP path of at most 4096 bytes");
      return false;
    }
    path = path_it->get<std::string>();
  } else if (operation == "sftp_read") {
    if (!has_only_keys(*parameters_it, {"path", "max_bytes"})) {
      set_error(error, 422, "invalid_parameters", "sftp_read accepts only path and max_bytes parameters");
      return false;
    }
    const auto path_it = parameters_it->find("path");
    const auto max_bytes_it = parameters_it->find("max_bytes");
    if (path_it == parameters_it->end() || !path_it->is_string() ||
        !valid_sftp_path(path_it->get<std::string>()) || max_bytes_it == parameters_it->end() ||
        !(max_bytes_it->is_number_integer() || max_bytes_it->is_number_unsigned())) {
      set_error(error, 422, "invalid_parameters", "sftp_read requires a valid path and max_bytes");
      return false;
    }
    std::uint64_t requested_bytes = 0;
    if (max_bytes_it->is_number_unsigned()) {
      requested_bytes = max_bytes_it->get<std::uint64_t>();
    } else {
      const auto signed_bytes = max_bytes_it->get<std::int64_t>();
      if (signed_bytes < 0) {
        set_error(error, 422, "invalid_parameters", "max_bytes must be between 0 and 1048576");
        return false;
      }
      requested_bytes = static_cast<std::uint64_t>(signed_bytes);
    }
    if (requested_bytes > 1024 * 1024) {
      set_error(error, 422, "invalid_parameters", "max_bytes must be between 0 and 1048576");
      return false;
    }
    path = path_it->get<std::string>();
    max_bytes = static_cast<std::uint32_t>(requested_bytes);
  } else if (operation == "sftp_write" || operation == "sftp_create_exclusive") {
    if (!has_only_keys(*parameters_it, {"path", "content_base64"})) {
      set_error(error, 422, "invalid_parameters", "SFTP write operations accept only path and content_base64");
      return false;
    }
    const auto path_it = parameters_it->find("path");
    const auto content_it = parameters_it->find("content_base64");
    const std::size_t maximum = operation == "sftp_write" ? 512 * 1024 : 1024 * 1024;
    if (path_it == parameters_it->end() || !path_it->is_string() ||
        !valid_sftp_path(path_it->get<std::string>()) || content_it == parameters_it->end() ||
        !content_it->is_string() ||
        !decode_base64(content_it->get_ref<const std::string&>(), maximum, &content)) {
      set_error(error, 422, "invalid_parameters", "path or base64 file contents are invalid or exceed the operation limit");
      return false;
    }
    path = path_it->get<std::string>();
  } else if (operation == "sftp_replace_guarded") {
    if (!has_only_keys(*parameters_it,
                       {"path", "expected_size", "expected_modified", "expected_digest", "content_base64"})) {
      set_error(error, 422, "invalid_parameters", "sftp_replace_guarded received an unexpected parameter");
      return false;
    }
    const auto path_it = parameters_it->find("path");
    const auto size_it = parameters_it->find("expected_size");
    const auto modified_it = parameters_it->find("expected_modified");
    const auto digest_it = parameters_it->find("expected_digest");
    const auto content_it = parameters_it->find("content_base64");
    if (path_it == parameters_it->end() || !path_it->is_string() ||
        !valid_sftp_path(path_it->get<std::string>()) ||
        path_it->get<std::string>().front() != '/' ||
        path_it->get<std::string>().find_first_of("\r\n") != std::string::npos ||
        size_it == parameters_it->end() ||
        !(size_it->is_number_integer() || size_it->is_number_unsigned()) ||
        modified_it == parameters_it->end() ||
        !(modified_it->is_null() || modified_it->is_number_integer() || modified_it->is_number_unsigned()) ||
        digest_it == parameters_it->end() || !digest_it->is_string() ||
        digest_it->get_ref<const std::string&>().size() != 64 ||
        content_it == parameters_it->end() || !content_it->is_string() ||
        !decode_base64(content_it->get_ref<const std::string&>(), 1024 * 1024, &content)) {
      set_error(error, 422, "invalid_parameters", "sftp_replace_guarded requires an absolute path, bounded contents, a size, optional mtime and SHA-256 digest");
      return false;
    }
    if (size_it->is_number_unsigned()) {
      expected_size = size_it->get<std::uint64_t>();
    } else {
      const auto signed_size = size_it->get<std::int64_t>();
      if (signed_size < 0) {
        set_error(error, 422, "invalid_parameters", "expected_size must be between 0 and 1048576");
        return false;
      }
      expected_size = static_cast<std::uint64_t>(signed_size);
    }
    if (expected_size > 1024 * 1024) {
      set_error(error, 422, "invalid_parameters", "expected_size must be between 0 and 1048576");
      return false;
    }
    if (!modified_it->is_null()) {
      std::uint64_t modified = 0;
      if (modified_it->is_number_unsigned()) {
        modified = modified_it->get<std::uint64_t>();
      } else {
        const auto signed_modified = modified_it->get<std::int64_t>();
        if (signed_modified < 0) {
          set_error(error, 422, "invalid_parameters", "expected_modified must be a non-negative SFTP timestamp or null");
          return false;
        }
        modified = static_cast<std::uint64_t>(signed_modified);
      }
      if (modified > std::numeric_limits<std::uint32_t>::max()) {
        set_error(error, 422, "invalid_parameters", "expected_modified is outside the SFTP timestamp range");
        return false;
      }
      expected_modified = static_cast<std::uint32_t>(modified);
    }
    const auto& digest = digest_it->get_ref<const std::string&>();
    for (std::size_t i = 0; i < expected_digest.size(); ++i) {
      const auto hex_value = [](char ch) -> int {
        if (ch >= '0' && ch <= '9') return ch - '0';
        if (ch >= 'a' && ch <= 'f') return ch - 'a' + 10;
        if (ch >= 'A' && ch <= 'F') return ch - 'A' + 10;
        return -1;
      };
      const int high = hex_value(digest[i * 2]);
      const int low = hex_value(digest[i * 2 + 1]);
      if (high < 0 || low < 0) {
        set_error(error, 422, "invalid_parameters", "expected_digest must be a 64-character SHA-256 hex string");
        return false;
      }
      expected_digest[i] = static_cast<unsigned char>((high << 4) | low);
    }
    path = path_it->get<std::string>();
  } else if (operation == "sftp_publish_no_replace") {
    if (!has_only_keys(*parameters_it, {"source", "destination"})) {
      set_error(error, 422, "invalid_parameters", "SFTP publication accepts only source and destination paths");
      return false;
    }
    const auto source_it = parameters_it->find("source");
    const auto destination_it = parameters_it->find("destination");
    if (source_it == parameters_it->end() || !source_it->is_string() ||
        destination_it == parameters_it->end() || !destination_it->is_string()) {
      set_error(error, 422, "invalid_parameters", "SFTP publication requires source and destination paths");
      return false;
    }
    source_path = source_it->get<std::string>();
    destination_path = destination_it->get<std::string>();
    const auto parent_path = [](const std::string& value) -> std::string_view {
      const auto separator = value.find_last_of('/');
      if (separator == std::string::npos) return {};
      return separator == 0 ? std::string_view(value).substr(0, 1)
                            : std::string_view(value).substr(0, separator);
    };
    const bool source_valid = valid_sftp_path(source_path) && source_path.front() == '/' &&
        source_path.find_first_of("\r\n") == std::string::npos;
    const bool destination_valid = valid_sftp_path(destination_path) &&
        destination_path.front() == '/' && destination_path.find_first_of("\r\n") == std::string::npos;
    if (!source_valid || !destination_valid || source_path == destination_path ||
        parent_path(source_path) != parent_path(destination_path)) {
      set_error(error, 422, "invalid_parameters", "SFTP no-replace publication requires distinct absolute sibling paths");
      return false;
    }
  } else if (operation == "sftp_remove_file" || operation == "sftp_link_count" ||
             operation == "sftp_create_dir") {
    if (!has_only_keys(*parameters_it, {"path"})) {
      set_error(error, 422, "invalid_parameters", "SFTP path operations accept only a path parameter");
      return false;
    }
    const auto path_it = parameters_it->find("path");
    if (path_it == parameters_it->end() || !path_it->is_string() ||
        !valid_sftp_path(path_it->get<std::string>())) {
      set_error(error, 422, "invalid_parameters", "path must be a non-empty SFTP path of at most 4096 bytes");
      return false;
    }
    if (operation == "sftp_link_count" &&
        (path_it->get<std::string>().front() != '/' ||
         path_it->get<std::string>().find_first_of("\r\n") != std::string::npos)) {
      set_error(error, 422, "invalid_parameters", "sftp_link_count requires an absolute path without line breaks");
      return false;
    }
    path = path_it->get<std::string>();
  } else {
    set_error(error, 422, "unsupported_operation", "The requested structured SSH operation is not enabled");
    return false;
  }
  std::uint64_t timeout = 0;
  if (timeout_it->is_number_unsigned()) {
    timeout = timeout_it->get<std::uint64_t>();
  } else {
    const auto signed_timeout = timeout_it->get<std::int64_t>();
    if (signed_timeout < 1 || signed_timeout > 30) {
      set_error(error, 422, "invalid_timeout", "timeout_seconds must be between 1 and 30");
      return false;
    }
    timeout = static_cast<std::uint64_t>(signed_timeout);
  }
  if (timeout < 1 || timeout > 30) {
    set_error(error, 422, "invalid_timeout", "timeout_seconds must be between 1 and 30");
    return false;
  }
  if (!request) {
    set_error(error, 500, "internal_error", "Request parser was not given an output value");
    return false;
  }
  request->target_id = target_id;
  request->operation = operation;
  request->service = std::move(service);
  request->unit = std::move(unit);
  request->path = std::move(path);
  request->source_path = std::move(source_path);
  request->destination_path = std::move(destination_path);
  request->nonce = std::move(nonce);
  request->package_manager = std::move(package_manager);
  request->package = std::move(package);
  request->container = std::move(container);
  request->include_all_containers = include_all_containers;
  request->tail = tail;
  request->content = std::move(content);
  request->since_seconds = since_seconds;
  request->expected_size = expected_size;
  request->expected_modified = expected_modified;
  request->expected_digest = expected_digest;
  request->max_bytes = max_bytes;
  request->timeout_seconds = static_cast<int>(timeout);
  return true;
}

}  // namespace yukinal::ssh_service

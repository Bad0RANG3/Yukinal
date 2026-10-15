#include "yukinal/desktop_host/network_proxy.hpp"

#include <algorithm>
#include <array>
#include <cctype>
#include <charconv>
#include <cstdlib>
#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#ifdef _WIN32
#include <windows.h>
#elif defined(__APPLE__)
#include <CoreFoundation/CoreFoundation.h>
#include <SystemConfiguration/SystemConfiguration.h>
#endif

namespace yukinal::desktop_host {
namespace {

struct PlatformProxy {
  std::optional<std::string> static_proxy;
  std::optional<std::string> no_proxy;
  std::optional<std::string> pac_url;
};

std::string trim(std::string_view value) {
  auto first = value.begin();
  auto last = value.end();
  while (first != last && std::isspace(static_cast<unsigned char>(*first))) ++first;
  while (last != first && std::isspace(static_cast<unsigned char>(*(last - 1)))) --last;
  return {first, last};
}

std::string lower_ascii(std::string value) {
  std::transform(value.begin(), value.end(), value.begin(), [](unsigned char byte) {
    return static_cast<char>(std::tolower(byte));
  });
  return value;
}

std::optional<std::string> non_empty(std::optional<std::string> value) {
  if (!value || trim(*value).empty()) return std::nullopt;
  return value;
}

template <std::size_t Count>
std::optional<std::string> first_environment_value(
    const std::array<const char*, Count>& names) {
  for (const auto* name : names) {
    if (const auto* raw = std::getenv(name); raw != nullptr) {
      if (auto value = non_empty(std::string(raw))) return value;
    }
  }
  return std::nullopt;
}

struct NormalizedProxy {
  std::optional<std::string> url;
  std::string reason;
};

NormalizedProxy normalize_proxy(std::string_view raw) {
  auto value = trim(raw);
  if (value.empty()) return {std::nullopt, "代理地址是空的"};
  if (std::any_of(value.begin(), value.end(), [](unsigned char byte) {
        return byte < 0x21 || byte == 0x7f;
      })) {
    return {std::nullopt, "代理地址不是合法 URL"};
  }

  if (value.find("://") == std::string::npos) value = "http://" + value;
  const auto scheme_end = value.find("://");
  const auto scheme = lower_ascii(value.substr(0, scheme_end));
  if (scheme != "http" && scheme != "https" && scheme != "socks5" &&
      scheme != "socks5h") {
    return {std::nullopt,
            "代理地址的 scheme 不支持；只支持 http、https、socks5 与 socks5h"};
  }
  value.replace(0, scheme_end, scheme);
  if (value.find('?') != std::string::npos || value.find('#') != std::string::npos) {
    return {std::nullopt, "代理地址不能带 query 或 fragment"};
  }

  const auto authority_begin = value.find("://") + 3;
  const auto path_begin = value.find('/', authority_begin);
  const auto authority_end = path_begin == std::string::npos ? value.size() : path_begin;
  const std::string_view authority(value.data() + authority_begin,
                                   authority_end - authority_begin);
  if (authority.empty()) return {std::nullopt, "代理地址没有主机名"};
  if (authority.find('@') != std::string_view::npos) {
    return {std::nullopt,
            "代理地址里内嵌了用户名或密码；请把凭据填在「代理凭据」里，它只进系统凭据库"};
  }

  std::string_view host;
  std::string_view port;
  if (authority.front() == '[') {
    const auto closing = authority.find(']');
    if (closing == std::string_view::npos || closing == 1) {
      return {std::nullopt, "代理地址不是合法 URL"};
    }
    host = authority.substr(0, closing + 1);
    const auto rest = authority.substr(closing + 1);
    if (!rest.empty()) {
      if (rest.front() != ':') return {std::nullopt, "代理地址不是合法 URL"};
      port = rest.substr(1);
    }
  } else {
    const auto colon = authority.rfind(':');
    if (colon != std::string_view::npos) {
      if (authority.find(':') != colon) return {std::nullopt, "代理地址不是合法 URL"};
      host = authority.substr(0, colon);
      port = authority.substr(colon + 1);
    } else {
      host = authority;
    }
  }
  if (host.empty() || std::any_of(host.begin(), host.end(), [](unsigned char byte) {
        return std::isspace(byte) || byte == '%' || byte == '[' || byte == ']' || byte == '@';
      })) {
    return {std::nullopt, "代理地址没有有效的主机名"};
  }
  if (authority.back() == ':' || !port.empty()) {
    unsigned int parsed_port = 0;
    const auto [end, error] = std::from_chars(port.data(), port.data() + port.size(), parsed_port);
    if (port.empty() || error != std::errc{} || end != port.data() + port.size() ||
        parsed_port == 0 || parsed_port > 65535) {
      return {std::nullopt, "代理地址不是合法 URL"};
    }
  }

  while (!value.empty() && value.back() == '/') value.pop_back();
  return {std::move(value), {}};
}

std::string windows_proxy_server(std::string_view raw) {
  const auto value = trim(raw);
  if (value.empty()) return {};
  if (value.find('=') == std::string::npos) return value;
  std::optional<std::string> fallback;
  std::size_t begin = 0;
  while (begin <= value.size()) {
    const auto end = value.find(';', begin);
    const auto entry = std::string_view(value).substr(
        begin, end == std::string::npos ? value.size() - begin : end - begin);
    const auto equals = entry.find('=');
    if (equals != std::string_view::npos) {
      const auto scheme = lower_ascii(trim(entry.substr(0, equals)));
      const auto endpoint = trim(entry.substr(equals + 1));
      if (!endpoint.empty()) {
        if (scheme == "https") return "https://" + endpoint;
        if (scheme == "http") fallback = "http://" + endpoint;
        else if (!fallback) fallback = "http://" + endpoint;
      }
    }
    if (end == std::string::npos) break;
    begin = end + 1;
  }
  return fallback.value_or(std::string());
}

std::optional<std::string> windows_no_proxy(std::string_view raw) {
  std::vector<std::string> entries;
  std::size_t begin = 0;
  while (begin <= raw.size()) {
    const auto end = raw.find(';', begin);
    const auto entry = trim(raw.substr(begin,
        end == std::string_view::npos ? raw.size() - begin : end - begin));
    if (!entry.empty() && lower_ascii(entry) != "<local>") entries.push_back(entry);
    if (end == std::string_view::npos) break;
    begin = end + 1;
  }
  if (entries.empty()) return std::nullopt;
  std::string result;
  for (const auto& entry : entries) {
    if (!result.empty()) result.push_back(',');
    result += entry;
  }
  return result;
}

#ifdef _WIN32
std::optional<std::string> read_registry_string(HKEY key, const wchar_t* name) {
  DWORD type = 0;
  DWORD bytes = 0;
  if (RegQueryValueExW(key, name, nullptr, &type, nullptr, &bytes) != ERROR_SUCCESS ||
      (type != REG_SZ && type != REG_EXPAND_SZ) || bytes < sizeof(wchar_t)) {
    return std::nullopt;
  }
  std::wstring value(bytes / sizeof(wchar_t), L'\0');
  if (RegQueryValueExW(key, name, nullptr, &type,
                       reinterpret_cast<BYTE*>(value.data()), &bytes) != ERROR_SUCCESS) {
    return std::nullopt;
  }
  while (!value.empty() && value.back() == L'\0') value.pop_back();
  const auto length = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
                                           static_cast<int>(value.size()), nullptr, 0,
                                           nullptr, nullptr);
  if (length <= 0) return std::nullopt;
  std::string encoded(static_cast<std::size_t>(length), '\0');
  if (WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
                          static_cast<int>(value.size()), encoded.data(), length,
                          nullptr, nullptr) <= 0) return std::nullopt;
  return encoded;
}

std::uint32_t read_registry_dword(HKEY key, const wchar_t* name) {
  DWORD value = 0;
  DWORD type = 0;
  DWORD bytes = sizeof(value);
  if (RegQueryValueExW(key, name, nullptr, &type,
                       reinterpret_cast<BYTE*>(&value), &bytes) != ERROR_SUCCESS ||
      type != REG_DWORD || bytes != sizeof(value)) return 0;
  return value;
}

std::optional<PlatformProxy> platform_proxy() {
  HKEY key = nullptr;
  if (RegOpenKeyExW(HKEY_CURRENT_USER,
                    L"Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings",
                    0, KEY_QUERY_VALUE, &key) != ERROR_SUCCESS) return std::nullopt;
  struct KeyOwner {
    HKEY key;
    ~KeyOwner() { if (key != nullptr) RegCloseKey(key); }
  } owner{key};
  const bool enabled = read_registry_dword(key, L"ProxyEnable") == 1;
  const auto server = read_registry_string(key, L"ProxyServer").value_or("");
  const auto overrides = read_registry_string(key, L"ProxyOverride").value_or("");
  auto pac = non_empty(read_registry_string(key, L"AutoConfigURL"));
  if (!pac && read_registry_dword(key, L"AutoDetect") == 1) {
    pac = "automatic proxy discovery";
  }
  PlatformProxy result;
  if (enabled) result.static_proxy = non_empty(windows_proxy_server(server));
  result.no_proxy = windows_no_proxy(overrides);
  result.pac_url = std::move(pac);
  return result;
}
#elif defined(__APPLE__)
std::optional<std::string> cf_string(CFTypeRef value) {
  if (value == nullptr || CFGetTypeID(value) != CFStringGetTypeID()) return std::nullopt;
  auto string = static_cast<CFStringRef>(value);
  const auto maximum = CFStringGetMaximumSizeForEncoding(
      CFStringGetLength(string), kCFStringEncodingUTF8);
  if (maximum < 0) return std::nullopt;
  std::string result(static_cast<std::size_t>(maximum) + 1, '\0');
  if (!CFStringGetCString(string, result.data(), maximum + 1, kCFStringEncodingUTF8)) {
    return std::nullopt;
  }
  result.resize(std::char_traits<char>::length(result.c_str()));
  return result;
}

bool cf_enabled(const void* dictionary, CFStringRef key) {
  const auto value = static_cast<CFTypeRef>(CFDictionaryGetValue(
      static_cast<CFDictionaryRef>(dictionary), key));
  if (value == nullptr) return false;
  if (CFGetTypeID(value) == CFBooleanGetTypeID()) return CFBooleanGetValue(
      static_cast<CFBooleanRef>(value));
  if (CFGetTypeID(value) == CFNumberGetTypeID()) {
    int number = 0;
    return CFNumberGetValue(static_cast<CFNumberRef>(value), kCFNumberIntType, &number) &&
           number != 0;
  }
  return false;
}

std::optional<int> cf_integer(const void* dictionary, CFStringRef key) {
  const auto value = static_cast<CFTypeRef>(CFDictionaryGetValue(
      static_cast<CFDictionaryRef>(dictionary), key));
  if (value == nullptr || CFGetTypeID(value) != CFNumberGetTypeID()) return std::nullopt;
  int number = 0;
  if (!CFNumberGetValue(static_cast<CFNumberRef>(value), kCFNumberIntType, &number)) {
    return std::nullopt;
  }
  return number;
}

std::optional<std::string> mac_proxy_endpoint(const void* dictionary,
                                              CFStringRef host_key,
                                              CFStringRef port_key) {
  auto host = non_empty(cf_string(CFDictionaryGetValue(
      static_cast<CFDictionaryRef>(dictionary), host_key)));
  if (!host) return std::nullopt;
  const auto port = cf_integer(dictionary, port_key);
  if (port && *port > 0 && *port <= 65535) {
    return "http://" + *host + ":" + std::to_string(*port);
  }
  return "http://" + *host;
}

std::optional<PlatformProxy> platform_proxy() {
  auto* proxies = SCDynamicStoreCopyProxies(nullptr);
  if (proxies == nullptr) return std::nullopt;
  struct DictionaryOwner {
    CFDictionaryRef value;
    ~DictionaryOwner() { if (value != nullptr) CFRelease(value); }
  } owner{proxies};
  PlatformProxy result;
  if (cf_enabled(proxies, kSCPropNetProxiesHTTPSEnable)) {
    result.static_proxy = mac_proxy_endpoint(proxies, kSCPropNetProxiesHTTPSProxy,
                                              kSCPropNetProxiesHTTPSPort);
    if (!result.static_proxy) {
      result.static_proxy = mac_proxy_endpoint(proxies, kSCPropNetProxiesHTTPProxy,
                                                kSCPropNetProxiesHTTPPort);
    }
  } else if (cf_enabled(proxies, kSCPropNetProxiesHTTPEnable)) {
    result.static_proxy = mac_proxy_endpoint(proxies, kSCPropNetProxiesHTTPProxy,
                                              kSCPropNetProxiesHTTPPort);
  }

  const auto exceptions_value = CFDictionaryGetValue(proxies,
                                                       kSCPropNetProxiesExceptionsList);
  if (exceptions_value != nullptr && CFGetTypeID(exceptions_value) == CFArrayGetTypeID()) {
    const auto exceptions = static_cast<CFArrayRef>(exceptions_value);
    std::string joined;
    for (CFIndex index = 0; index < CFArrayGetCount(exceptions); ++index) {
      const auto item = cf_string(CFArrayGetValueAtIndex(exceptions, index));
      if (!item || item->empty()) continue;
      if (!joined.empty()) joined.push_back(',');
      joined += *item;
    }
    result.no_proxy = non_empty(std::move(joined));
  }

  if (cf_enabled(proxies, kSCPropNetProxiesProxyAutoConfigEnable)) {
    result.pac_url = cf_string(CFDictionaryGetValue(
        proxies, kSCPropNetProxiesProxyAutoConfigURLString));
    result.pac_url = non_empty(std::move(result.pac_url));
  }
  if (!result.pac_url && cf_enabled(proxies, kSCPropNetProxiesProxyAutoDiscoveryEnable)) {
    result.pac_url = "automatic proxy discovery";
  }
  return result;
}
#else
std::optional<PlatformProxy> platform_proxy() { return std::nullopt; }
#endif

nlohmann::json describe_proxy(const std::optional<std::string>& raw,
                             const std::string& source,
                             const std::optional<std::string>& no_proxy,
                             const std::optional<std::string>& pac) {
  if (raw) {
    auto normalized = normalize_proxy(*raw);
    if (!normalized.url) {
      return {{"kind", "unusable"},
              {"reason", normalized.reason + "（来自" + source + "）"}};
    }
    return {{"kind", "proxy"}, {"url", *normalized.url}, {"source", source},
            {"hasNoProxy", no_proxy.has_value()}};
  }
  if (pac) {
    return {{"kind", "unusable"},
            {"reason", "系统代理只配置了 PAC：PAC 是一段 JavaScript，本应用不执行它。"
                       "请在设置里选「直连」，或把静态代理写进 HTTPS_PROXY 环境变量"}};
  }
  return {{"kind", "direct"}};
}

}  // namespace

nlohmann::json resolve_network_proxy(bool system_mode) {
  if (!system_mode) return {{"kind", "direct"}};
  constexpr std::array<const char*, 6> proxy_names = {
      "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"};
  if (auto environment_proxy = first_environment_value(proxy_names)) {
    constexpr std::array<const char*, 2> no_proxy_names = {"NO_PROXY", "no_proxy"};
    return describe_proxy(environment_proxy, "环境变量",
                          first_environment_value(no_proxy_names), std::nullopt);
  }
  if (auto system_proxy = platform_proxy()) {
#ifdef _WIN32
    constexpr const char* source = "Windows Internet 设置";
#elif defined(__APPLE__)
    constexpr const char* source = "macOS 网络设置";
#else
    constexpr const char* source = "环境变量";
#endif
    return describe_proxy(system_proxy->static_proxy, source,
                          system_proxy->no_proxy, system_proxy->pac_url);
  }
  return {{"kind", "direct"}};
}

}  // namespace yukinal::desktop_host

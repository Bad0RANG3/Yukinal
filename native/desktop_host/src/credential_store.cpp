#include "yukinal/desktop_host/credential_store.hpp"

#include <algorithm>
#include <cerrno>
#include <limits>
#include <memory>
#include <new>
#include <optional>
#include <utility>

#ifdef _WIN32
#include <windows.h>
#include <wincred.h>
#elif defined(__APPLE__)
#include <Security/Security.h>
#elif defined(__linux__)
#include <linux/keyctl.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <libsecret/secret.h>
#endif

namespace yukinal::desktop_host {
namespace {

void validate_component(const std::string& value, const char* description) {
  if (value.empty() || value.find('/') != std::string::npos ||
      value.find('\0') != std::string::npos) {
    throw CredentialStoreError(std::string("Credential ") + description + " is invalid");
  }
}

std::string os_error(const char* operation, long code) {
  return std::string("The operating system credential store could not ") + operation +
         " (error " + std::to_string(code) + ")";
}

#ifdef _WIN32
struct WindowsCredentialDeleter {
  void operator()(CREDENTIALW* credential) const noexcept {
    if (credential == nullptr) return;
    if (credential->CredentialBlob != nullptr) {
      SecureZeroMemory(credential->CredentialBlob, credential->CredentialBlobSize);
    }
    CredFree(credential);
  }
};

std::wstring utf8_to_wide(const std::string& value) {
  if (value.empty()) return {};
  if (value.size() > static_cast<std::size_t>(std::numeric_limits<int>::max())) {
    throw CredentialStoreError("Credential metadata exceeds the Windows API size limit");
  }
  const auto length = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                                          static_cast<int>(value.size()), nullptr, 0);
  if (length <= 0) throw CredentialStoreError("Credential metadata is not valid UTF-8");
  std::wstring converted(static_cast<std::size_t>(length), L'\0');
  if (MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                          static_cast<int>(value.size()), converted.data(), length) <= 0) {
    throw CredentialStoreError("Credential metadata is not valid UTF-8");
  }
  return converted;
}

std::wstring windows_target(const CredentialReference& reference) {
  return utf8_to_wide(reference.account + "." + reference.service);
}

PCREDENTIALW read_windows_credential(const CredentialReference& reference) {
  PCREDENTIALW credential = nullptr;
  const auto target = windows_target(reference);
  if (CredReadW(target.c_str(), CRED_TYPE_GENERIC, 0, &credential)) return credential;
  const auto error = GetLastError();
  if (error == ERROR_NOT_FOUND) return nullptr;
  throw CredentialStoreError(os_error("read a credential", error));
}
#elif defined(__APPLE__)
struct KeychainItemOwner {
  SecKeychainItemRef value;
  ~KeychainItemOwner() {
    if (value != nullptr) CFRelease(value);
  }
};

struct KeychainContentOwner {
  void* value;
  UInt32 length;
  ~KeychainContentOwner() {
    if (value == nullptr) return;
    volatile std::uint8_t* bytes = static_cast<volatile std::uint8_t*>(value);
    for (UInt32 index = 0; index < length; ++index) bytes[index] = 0;
    SecKeychainItemFreeContent(nullptr, value);
  }
};

void check_keychain_status(OSStatus status, const char* operation) {
  if (status == errSecSuccess) return;
  throw CredentialStoreError(os_error(operation, status));
}

SecKeychainItemRef find_keychain_item(const CredentialReference& reference,
                                     UInt32* secret_length = nullptr,
                                     void** secret_data = nullptr) {
  if (reference.service.size() > std::numeric_limits<UInt32>::max() ||
      reference.account.size() > std::numeric_limits<UInt32>::max()) {
    throw CredentialStoreError("Credential metadata exceeds the Keychain API size limit");
  }
  SecKeychainItemRef item = nullptr;
  const auto status = SecKeychainFindGenericPassword(
      nullptr, static_cast<UInt32>(reference.service.size()), reference.service.data(),
      static_cast<UInt32>(reference.account.size()), reference.account.data(),
      secret_length, secret_data, &item);
  if (status == errSecItemNotFound) return nullptr;
  check_keychain_status(status, "read a credential");
  return item;
}
#elif defined(__linux__)
const SecretSchema kCredentialSchema = {
    "org.freedesktop.Secret.Generic",
    SECRET_SCHEMA_NONE,
    {{"service", SECRET_SCHEMA_ATTRIBUTE_STRING},
     {"username", SECRET_SCHEMA_ATTRIBUTE_STRING},
     {"target", SECRET_SCHEMA_ATTRIBUTE_STRING},
     {"application", SECRET_SCHEMA_ATTRIBUTE_STRING}},
};

struct SecretServicePasswordDeleter {
  std::size_t length;
  void operator()(gchar* password) const noexcept {
    if (password == nullptr) return;
    volatile gchar* value = password;
    for (std::size_t index = 0; index < length; ++index) value[index] = 0;
    secret_password_free(password);
  }
};

std::string linux_key_description(const CredentialReference& reference) {
  return "keyring-rs:" + reference.account + "@" + reference.service;
}

bool is_missing_linux_key(int error) {
  if (error == ENOKEY) return true;
#ifdef EKEYEXPIRED
  if (error == EKEYEXPIRED) return true;
#endif
#ifdef EKEYREVOKED
  if (error == EKEYREVOKED) return true;
#endif
  return false;
}

bool linux_keyutils_get(const CredentialReference& reference,
                        std::vector<std::uint8_t>* secret) {
  const auto description = linux_key_description(reference);
  errno = 0;
  const auto key = syscall(__NR_keyctl, KEYCTL_SEARCH, KEY_SPEC_SESSION_KEYRING,
                           "user", description.c_str(), 0);
  if (key < 0) {
    const auto error = errno;
    if (is_missing_linux_key(error)) return false;
    throw CredentialStoreError(os_error("read the Linux session keyring", error));
  }

  errno = 0;
  const auto size = syscall(__NR_keyctl, KEYCTL_READ, key, nullptr, 0);
  if (size < 0) {
    throw CredentialStoreError(os_error("read the Linux session keyring", errno));
  }
  secret->resize(static_cast<std::size_t>(size));
  if (size == 0) return true;

  errno = 0;
  const auto read_size = syscall(__NR_keyctl, KEYCTL_READ, key, secret->data(),
                                 static_cast<std::size_t>(size));
  if (read_size < 0 || read_size != size) {
    const auto error = read_size < 0 ? errno : EIO;
    std::fill(secret->begin(), secret->end(), 0);
    secret->clear();
    throw CredentialStoreError(os_error("read the Linux session keyring", error));
  }
  return true;
}

void linux_keyutils_set(const CredentialReference& reference,
                        const std::uint8_t* secret, std::size_t size) {
  if (size == 0) {
    throw CredentialStoreError("The Linux session keyring does not accept empty credentials");
  }
  const auto description = linux_key_description(reference);
  errno = 0;
  const auto key = syscall(__NR_add_key, "user", description.c_str(), secret, size,
                           KEY_SPEC_SESSION_KEYRING);
  if (key < 0) {
    throw CredentialStoreError(os_error("save to the Linux session keyring", errno));
  }
}

void linux_keyutils_erase(const CredentialReference& reference) noexcept {
  try {
    const auto description = linux_key_description(reference);
    errno = 0;
    const auto key = syscall(__NR_keyctl, KEYCTL_SEARCH, KEY_SPEC_SESSION_KEYRING,
                             "user", description.c_str(), 0);
    if (key < 0) return;
    (void)syscall(__NR_keyctl, KEYCTL_INVALIDATE, key);
  } catch (...) {
  }
}

void restore_linux_keyutils(const CredentialReference& reference,
                            const std::optional<Secret>& previous) noexcept {
  try {
    if (previous && previous->size() != 0) {
      linux_keyutils_set(reference, previous->data(), previous->size());
    } else {
      linux_keyutils_erase(reference);
    }
  } catch (...) {
    // Preserve the Secret Service failure as the primary error.
  }
}

void check_secret_service_error(GError* error, const char* operation) {
  if (error == nullptr) return;
  const auto code = error->code;
  g_error_free(error);
  throw CredentialStoreError(os_error(operation, code));
}

void validate_linux_secret(const Secret& secret) {
  if (secret.size() == 0) {
    throw CredentialStoreError("The Linux session keyring does not accept empty credentials");
  }
  if (secret.size() > static_cast<std::size_t>(std::numeric_limits<gssize>::max())) {
    throw CredentialStoreError("Credential exceeds the Secret Service API size limit");
  }
  if (std::find(secret.bytes().begin(), secret.bytes().end(), 0) != secret.bytes().end()) {
    throw CredentialStoreError("The Secret Service backend only accepts UTF-8 credentials without NUL bytes");
  }
  if (!g_utf8_validate(reinterpret_cast<const gchar*>(secret.data()),
                       static_cast<gssize>(secret.size()), nullptr)) {
    throw CredentialStoreError("The Secret Service backend only accepts UTF-8 credentials");
  }
}

GHashTable* secret_attributes(const CredentialReference& reference) {
  auto* attributes = g_hash_table_new(g_str_hash, g_str_equal);
  if (attributes == nullptr) throw std::bad_alloc();
  g_hash_table_insert(attributes, const_cast<gchar*>("service"),
                      const_cast<gchar*>(reference.service.c_str()));
  g_hash_table_insert(attributes, const_cast<gchar*>("username"),
                      const_cast<gchar*>(reference.account.c_str()));
  g_hash_table_insert(attributes, const_cast<gchar*>("target"),
                      const_cast<gchar*>("default"));
  g_hash_table_insert(attributes, const_cast<gchar*>("application"),
                      const_cast<gchar*>("rust-keyring"));
  return attributes;
}

void free_secret_attributes(GHashTable* attributes) {
  if (attributes != nullptr) g_hash_table_unref(attributes);
}
#endif

}  // namespace

Secret::Secret(std::vector<std::uint8_t> bytes) : bytes_(std::move(bytes)) {}

Secret::Secret(std::string_view utf8)
    : bytes_(utf8.begin(), utf8.end()) {}

Secret::~Secret() { clear(); }

Secret::Secret(Secret&& other) noexcept : bytes_(std::move(other.bytes_)) {
  other.clear();
}

Secret& Secret::operator=(Secret&& other) noexcept {
  if (this == &other) return *this;
  clear();
  bytes_ = std::move(other.bytes_);
  other.clear();
  return *this;
}

void Secret::clear() noexcept {
  volatile std::uint8_t* value = bytes_.data();
  for (std::size_t index = 0; index < bytes_.size(); ++index) value[index] = 0;
  bytes_.clear();
}

CredentialReference CredentialReference::parse(std::string_view value) {
  constexpr std::string_view prefix = "keychain://";
  if (!value.starts_with(prefix)) {
    throw CredentialStoreError("Credential reference has an unsupported scheme");
  }
  value.remove_prefix(prefix.size());
  const auto slash = value.find('/');
  if (slash == std::string_view::npos || slash == 0 || slash + 1 == value.size() ||
      value.find('/', slash + 1) != std::string_view::npos) {
    throw CredentialStoreError("Credential reference is malformed");
  }
  CredentialReference reference{std::string(value.substr(0, slash)),
                                std::string(value.substr(slash + 1))};
  validate_component(reference.service, "service");
  validate_component(reference.account, "account");
  return reference;
}

std::string CredentialReference::to_string() const {
  validate_component(service, "service");
  validate_component(account, "account");
  return "keychain://" + service + "/" + account;
}

CredentialReference DesktopCredentialStore::set(const std::string& service,
                                                 const std::string& account,
                                                 const Secret& secret) const {
  validate_component(service, "service");
  validate_component(account, "account");
  const CredentialReference reference{service, account};
#ifdef _WIN32
  if (secret.size() > CRED_MAX_CREDENTIAL_BLOB_SIZE) {
    throw CredentialStoreError("The Windows credential store limits secrets to 2560 bytes");
  }
  const auto target = windows_target(reference);
  const auto username = utf8_to_wide(account);
  CREDENTIALW credential{};
  credential.Type = CRED_TYPE_GENERIC;
  credential.TargetName = const_cast<LPWSTR>(target.c_str());
  credential.Comment = const_cast<LPWSTR>(L"keyring v3.6.3");
  credential.CredentialBlobSize = static_cast<DWORD>(secret.size());
  credential.CredentialBlob = const_cast<LPBYTE>(secret.data());
  credential.Persist = CRED_PERSIST_ENTERPRISE;
  credential.UserName = const_cast<LPWSTR>(username.c_str());
  if (!CredWriteW(&credential, 0)) {
    throw CredentialStoreError(os_error("save a credential", GetLastError()));
  }
#elif defined(__APPLE__)
  if (service.size() > std::numeric_limits<UInt32>::max() ||
      account.size() > std::numeric_limits<UInt32>::max() ||
      secret.size() > std::numeric_limits<UInt32>::max()) {
    throw CredentialStoreError("Credential exceeds the Keychain API size limit");
  }
  SecKeychainItemRef item = nullptr;
  auto status = SecKeychainAddGenericPassword(
      nullptr, static_cast<UInt32>(service.size()), service.data(),
      static_cast<UInt32>(account.size()), account.data(),
      static_cast<UInt32>(secret.size()), secret.data(), &item);
  if (status == errSecDuplicateItem) {
    UInt32 previous_length = 0;
    void* previous_data = nullptr;
    item = find_keychain_item(reference, &previous_length, &previous_data);
    if (previous_data != nullptr) {
      volatile std::uint8_t* previous = static_cast<volatile std::uint8_t*>(previous_data);
      for (UInt32 index = 0; index < previous_length; ++index) previous[index] = 0;
      SecKeychainItemFreeContent(nullptr, previous_data);
    }
    if (item == nullptr) throw CredentialStoreError(os_error("find an existing credential", status));
    status = SecKeychainItemModifyAttributesAndData(
        item, nullptr, static_cast<UInt32>(secret.size()), secret.data());
  }
  if (item != nullptr) CFRelease(item);
  check_keychain_status(status, "save a credential");
#elif defined(__linux__)
  validate_linux_secret(secret);
  const auto label = account + "@" + service + ":default (keyring v3.6.3)";
  auto* attributes = secret_attributes(reference);
  std::vector<std::uint8_t> previous_value;
  std::optional<Secret> previous;
  try {
    if (linux_keyutils_get(reference, &previous_value)) {
      previous.emplace(std::move(previous_value));
    }
  } catch (...) {
  }
  try {
    linux_keyutils_set(reference, secret.data(), secret.size());
  } catch (...) {
    free_secret_attributes(attributes);
    throw;
  }
  GError* error = nullptr;
  const auto success = secret_password_storev_sync(
      &kCredentialSchema, attributes, SECRET_COLLECTION_DEFAULT, label.c_str(),
      reinterpret_cast<const gchar*>(secret.data()), nullptr, &error);
  free_secret_attributes(attributes);
  if (!success) {
    const auto code = error == nullptr ? 0 : error->code;
    if (error != nullptr) g_error_free(error);
    restore_linux_keyutils(reference, previous);
    throw CredentialStoreError(os_error("save a credential", code));
  }
  if (error != nullptr) g_error_free(error);
#else
  (void)secret;
  throw CredentialStoreError("OS credential storage is not implemented for this platform");
#endif
  return reference;
}

Secret DesktopCredentialStore::get(const CredentialReference& reference) const {
  validate_component(reference.service, "service");
  validate_component(reference.account, "account");
#ifdef _WIN32
  auto* credential = read_windows_credential(reference);
  if (credential == nullptr) throw CredentialNotFoundError("Credential was not found");
  std::unique_ptr<CREDENTIALW, WindowsCredentialDeleter> credential_owner(credential);
  std::vector<std::uint8_t> secret;
  if (credential->CredentialBlobSize != 0) {
    secret.assign(credential->CredentialBlob,
                  credential->CredentialBlob + credential->CredentialBlobSize);
  }
  return Secret(std::move(secret));
#elif defined(__APPLE__)
  UInt32 secret_length = 0;
  void* secret_data = nullptr;
  KeychainItemOwner item{find_keychain_item(reference, &secret_length, &secret_data)};
  if (item.value == nullptr) throw CredentialNotFoundError("Credential was not found");
  KeychainContentOwner content{secret_data, secret_length};
  if (secret_length != 0 && secret_data == nullptr) {
    throw CredentialStoreError("The Keychain returned an invalid credential buffer");
  }
  std::vector<std::uint8_t> secret;
  if (secret_length != 0) {
    secret.assign(static_cast<std::uint8_t*>(secret_data),
                  static_cast<std::uint8_t*>(secret_data) + secret_length);
  }
  return Secret(std::move(secret));
#elif defined(__linux__)
  std::vector<std::uint8_t> cached_secret;
  try {
    if (linux_keyutils_get(reference, &cached_secret)) {
      return Secret(std::move(cached_secret));
    }
  } catch (...) {
    std::fill(cached_secret.begin(), cached_secret.end(), 0);
    cached_secret.clear();
  }

  auto* attributes = secret_attributes(reference);
  GError* error = nullptr;
  gchar* password = secret_password_lookupv_sync(
      &kCredentialSchema, attributes, nullptr, &error);
  free_secret_attributes(attributes);
  const auto password_length = password == nullptr
                                   ? 0
                                   : std::char_traits<char>::length(password);
  std::unique_ptr<gchar, SecretServicePasswordDeleter> password_owner(
      password, SecretServicePasswordDeleter{password_length});
  if (error != nullptr) check_secret_service_error(error, "read a credential");
  if (password == nullptr) throw CredentialNotFoundError("Credential was not found");
  std::vector<std::uint8_t> secret(password, password + password_length);
  Secret result(std::move(secret));
  linux_keyutils_set(reference, result.data(), result.size());
  return result;
#else
  throw CredentialStoreError("OS credential storage is not implemented for this platform");
#endif
}

bool DesktopCredentialStore::has(const CredentialReference& reference) const {
  try {
    auto secret = get(reference);
    return true;
  } catch (const CredentialNotFoundError&) {
    return false;
  }
}

void DesktopCredentialStore::erase(const CredentialReference& reference) const {
  validate_component(reference.service, "service");
  validate_component(reference.account, "account");
#ifdef _WIN32
  const auto target = windows_target(reference);
  if (CredDeleteW(target.c_str(), CRED_TYPE_GENERIC, 0)) return;
  const auto error = GetLastError();
  if (error != ERROR_NOT_FOUND) throw CredentialStoreError(os_error("delete a credential", error));
#elif defined(__APPLE__)
  auto item = find_keychain_item(reference);
  if (item == nullptr) return;
  const auto status = SecKeychainItemDelete(item);
  CFRelease(item);
  check_keychain_status(status, "delete a credential");
#elif defined(__linux__)
  linux_keyutils_erase(reference);
  auto* attributes = secret_attributes(reference);
  GError* error = nullptr;
  const auto success = secret_password_clearv_sync(
      &kCredentialSchema, attributes, nullptr, &error);
  free_secret_attributes(attributes);
  if (!success) check_secret_service_error(error, "delete a credential");
  if (error != nullptr) g_error_free(error);
#else
  throw CredentialStoreError("OS credential storage is not implemented for this platform");
#endif
}

}  // namespace yukinal::desktop_host

#pragma once

#include <exception>
#include <functional>
#include <iostream>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

namespace yukinal::ssh_service::test {

struct Case {
  std::string name;
  std::function<void()> run;
};

inline std::vector<Case>& cases() {
  static std::vector<Case> value;
  return value;
}

struct Register {
  Register(std::string name, std::function<void()> run) {
    cases().push_back({std::move(name), std::move(run)});
  }
};

inline void require(bool condition, const char* expression, const char* file, int line) {
  if (condition) return;
  throw std::runtime_error(std::string(file) + ":" + std::to_string(line) + " check failed: " + expression);
}

}  // namespace yukinal::ssh_service::test

#define YUKINAL_TEST(name)                                      \
  static void name();                                           \
  static ::yukinal::ssh_service::test::Register name##_case(#name, name); \
  static void name()

#define YUKINAL_REQUIRE(expression) \
  ::yukinal::ssh_service::test::require((expression), #expression, __FILE__, __LINE__)

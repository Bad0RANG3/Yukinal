#include "test_support.hpp"

int main() {
  int failures = 0;
  for (const auto& test : yukinal::ssh_service::test::cases()) {
    try {
      test.run();
      std::cout << "PASS " << test.name << '\n';
    } catch (const std::exception& error) {
      ++failures;
      std::cerr << "FAIL " << test.name << ": " << error.what() << '\n';
    }
  }
  std::cout << yukinal::ssh_service::test::cases().size() << " test(s), " << failures << " failure(s)\n";
  return failures == 0 ? 0 : 1;
}

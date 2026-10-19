#include "test_support.hpp"
#include <cstdlib>

int main(int argc, char** argv) {
  if (argc == 2 && std::string(argv[1]) == "--credential-inheritance-check") {
    const char* value = std::getenv("YUKINAL_SSH_OVERRIDE_TEST_PASSWORD");
    return value != nullptr && std::string(value) == "local-test-process-baseline" ? 0 : 2;
  }
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

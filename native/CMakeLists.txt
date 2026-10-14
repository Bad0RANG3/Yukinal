cmake_minimum_required(VERSION 3.24)
project(yukinal_ssh_service VERSION 0.1.0 LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

if(MSVC)
  # The desktop installer must run on machines without a separately installed
  # Visual C++ Redistributable; keep the standalone service self-contained.
  set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>" CACHE STRING "" FORCE)
endif()

option(YUKINAL_BUILD_TESTS "Build the SSH service tests" ON)
option(YUKINAL_FETCH_DEPS "Fetch pinned third-party source dependencies" ON)
option(YUKINAL_ENABLE_SSH_MOCK_TEST "Register the Paramiko SSH mock test when Paramiko is installed" ON)
option(YUKINAL_ENABLE_SSH_CONTAINER_TEST "Register the real OpenSSH container integration test" OFF)
option(YUKINAL_ENABLE_AGENT_TESTS "Register Python Agent Runtime tests when dependencies are installed" ON)

if(YUKINAL_FETCH_DEPS)
  include(FetchContent)

  set(JSON_BuildTests OFF CACHE INTERNAL "")
  FetchContent_Declare(
    nlohmann_json
    GIT_REPOSITORY https://github.com/nlohmann/json.git
    GIT_TAG v3.11.3
    GIT_SHALLOW TRUE
  )
  FetchContent_MakeAvailable(nlohmann_json)

  set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
  set(BUILD_STATIC_LIBS ON CACHE BOOL "" FORCE)
  set(BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
  set(BUILD_TESTING OFF CACHE BOOL "" FORCE)
  set(ENABLE_ZLIB_COMPRESSION OFF CACHE BOOL "" FORCE)
  if(WIN32)
    set(CRYPTO_BACKEND WinCNG CACHE STRING "" FORCE)
    set(ENABLE_ECDSA_WINCNG ON CACHE BOOL "" FORCE)
  else()
    # The service is shipped as a standalone desktop resource. Prefer static OpenSSL
    # archives so the installed service does not depend on a developer's OpenSSL path.
    set(OPENSSL_USE_STATIC_LIBS TRUE)
    set(CRYPTO_BACKEND OpenSSL CACHE STRING "" FORCE)
  endif()
  FetchContent_Declare(
    libssh2
    GIT_REPOSITORY https://github.com/libssh2/libssh2.git
    GIT_TAG libssh2-1.11.1
    GIT_SHALLOW TRUE
  )
  FetchContent_MakeAvailable(libssh2)

  FetchContent_Declare(
    cpp_httplib
    GIT_REPOSITORY https://github.com/yhirose/cpp-httplib.git
    GIT_TAG v0.18.3
    GIT_SHALLOW TRUE
  )
  set(HTTPLIB_COMPILE OFF CACHE BOOL "" FORCE)
  set(HTTPLIB_INSTALL OFF CACHE BOOL "" FORCE)
  set(HTTPLIB_TEST OFF CACHE BOOL "" FORCE)
  set(HTTPLIB_USE_OPENSSL_IF_AVAILABLE OFF CACHE BOOL "" FORCE)
  set(HTTPLIB_USE_WOLFSSL_IF_AVAILABLE OFF CACHE BOOL "" FORCE)
  set(HTTPLIB_USE_MBEDTLS_IF_AVAILABLE OFF CACHE BOOL "" FORCE)
  set(HTTPLIB_USE_ZLIB_IF_AVAILABLE OFF CACHE BOOL "" FORCE)
  set(HTTPLIB_USE_BROTLI_IF_AVAILABLE OFF CACHE BOOL "" FORCE)
  set(HTTPLIB_USE_ZSTD_IF_AVAILABLE OFF CACHE BOOL "" FORCE)
  FetchContent_MakeAvailable(cpp_httplib)
else()
  find_package(nlohmann_json 3.11 CONFIG REQUIRED)
  find_package(libssh2 CONFIG REQUIRED)
  find_path(HTTPLIB_INCLUDE_DIR httplib.h REQUIRED)
endif()

find_package(Threads REQUIRED)

add_library(yukinal_ssh_core
  src/config.cpp
  src/policy.cpp
  src/ssh.cpp
  src/http_service.cpp
)
target_include_directories(yukinal_ssh_core PUBLIC include)
if(YUKINAL_FETCH_DEPS)
  target_include_directories(yukinal_ssh_core PRIVATE ${cpp_httplib_SOURCE_DIR})
else()
  target_include_directories(yukinal_ssh_core PRIVATE ${HTTPLIB_INCLUDE_DIR})
endif()
target_link_libraries(yukinal_ssh_core
  PUBLIC nlohmann_json::nlohmann_json
  PRIVATE libssh2::libssh2 Threads::Threads
)
if(WIN32)
  target_compile_definitions(yukinal_ssh_core PRIVATE NOMINMAX WIN32_LEAN_AND_MEAN)
  target_link_libraries(yukinal_ssh_core PRIVATE ws2_32 bcrypt)
else()
  find_package(OpenSSL REQUIRED COMPONENTS Crypto)
  target_link_libraries(yukinal_ssh_core PRIVATE OpenSSL::Crypto)
endif()

add_executable(yukinal-ssh-service src/main.cpp)
target_link_libraries(yukinal-ssh-service PRIVATE yukinal_ssh_core)

if(YUKINAL_BUILD_TESTS)
  enable_testing()
  add_executable(yukinal_ssh_tests
    tests/test_main.cpp
    tests/test_config.cpp
    tests/test_policy.cpp
    tests/test_network.cpp
  )
  target_include_directories(yukinal_ssh_tests PRIVATE tests src)
  target_link_libraries(yukinal_ssh_tests PRIVATE yukinal_ssh_core)
  add_test(NAME yukinal_ssh_tests COMMAND yukinal_ssh_tests)

  find_package(Python3 3.11 COMPONENTS Interpreter REQUIRED)
  add_test(
    NAME python_client_tests
    COMMAND ${Python3_EXECUTABLE} -m unittest discover -s ${CMAKE_CURRENT_SOURCE_DIR}/../agent/tests -p test_client.py -v
  )
  set_tests_properties(python_client_tests PROPERTIES
    ENVIRONMENT "PYTHONPATH=${CMAKE_CURRENT_SOURCE_DIR}/../agent/src"
  )

  add_test(
    NAME service_http_integration
    COMMAND ${Python3_EXECUTABLE} -m unittest discover -s ${CMAKE_CURRENT_SOURCE_DIR}/../agent/tests -p test_service_integration.py -v
  )
  set_tests_properties(service_http_integration PROPERTIES
    ENVIRONMENT "PYTHONPATH=${CMAKE_CURRENT_SOURCE_DIR}/../agent/src;YUKINAL_SSH_SERVICE_BINARY=$<TARGET_FILE:yukinal-ssh-service>"
    TIMEOUT 30
  )

  if(YUKINAL_ENABLE_AGENT_TESTS)
    execute_process(
      COMMAND ${Python3_EXECUTABLE} -c "import jsonschema, langgraph, mcp; import langgraph.checkpoint.sqlite.aio"
      RESULT_VARIABLE YUKINAL_AGENT_IMPORT_RESULT
      OUTPUT_QUIET
      ERROR_QUIET
    )
    if(YUKINAL_AGENT_IMPORT_RESULT EQUAL 0)
      add_test(
        NAME python_agent_tests
        COMMAND ${Python3_EXECUTABLE} -m unittest discover -s ${CMAKE_CURRENT_SOURCE_DIR}/../agent/tests -p test_agent_*.py -v
      )
      set_tests_properties(python_agent_tests PROPERTIES
        ENVIRONMENT "PYTHONPATH=${CMAKE_CURRENT_SOURCE_DIR}/../agent/src"
        TIMEOUT 90
      )
    else()
      message(STATUS "Agent dependencies are not installed; python_agent_tests is not registered (install agent[agent])")
    endif()
  endif()

  if(YUKINAL_ENABLE_SSH_MOCK_TEST)
    execute_process(
      COMMAND ${Python3_EXECUTABLE} -c "import paramiko"
      RESULT_VARIABLE YUKINAL_PARAMIKO_IMPORT_RESULT
      OUTPUT_QUIET
      ERROR_QUIET
    )
    if(YUKINAL_PARAMIKO_IMPORT_RESULT EQUAL 0)
      add_test(
        NAME ssh_mock_integration
        COMMAND ${Python3_EXECUTABLE} ${CMAKE_CURRENT_SOURCE_DIR}/../scripts/test_ssh_mock.py
                --service-binary $<TARGET_FILE:yukinal-ssh-service>
      )
      set_tests_properties(ssh_mock_integration PROPERTIES
        ENVIRONMENT_MODIFICATION "PYTHONPATH=path_list_prepend:${CMAKE_CURRENT_SOURCE_DIR}/../agent/src"
        TIMEOUT 60
      )

      execute_process(
        COMMAND ${Python3_EXECUTABLE} -c "import jsonschema, langgraph, mcp; import langgraph.checkpoint.sqlite.aio"
        RESULT_VARIABLE YUKINAL_AGENT_MOCK_IMPORT_RESULT
        OUTPUT_QUIET
        ERROR_QUIET
      )
      if(YUKINAL_AGENT_MOCK_IMPORT_RESULT EQUAL 0)
        add_test(
          NAME agent_ssh_mock_integration
          COMMAND ${Python3_EXECUTABLE} ${CMAKE_CURRENT_SOURCE_DIR}/../scripts/test_ssh_mock.py
                  --service-binary $<TARGET_FILE:yukinal-ssh-service> --agent-runtime-only
        )
        set_tests_properties(agent_ssh_mock_integration PROPERTIES
          ENVIRONMENT_MODIFICATION "PYTHONPATH=path_list_prepend:${CMAKE_CURRENT_SOURCE_DIR}/../agent/src"
          TIMEOUT 60
        )
      else()
        message(STATUS "Agent dependencies are not installed; agent_ssh_mock_integration is not registered")
      endif()
    else()
      message(STATUS "Paramiko is not installed; ssh_mock_integration is not registered (install agent[test])")
    endif()
  endif()

  if(YUKINAL_ENABLE_SSH_CONTAINER_TEST)
    add_test(
      NAME openssh_container_integration
      COMMAND ${Python3_EXECUTABLE} ${CMAKE_CURRENT_SOURCE_DIR}/../scripts/test_ssh_container.py
              --service-binary $<TARGET_FILE:yukinal-ssh-service>
    )
    set_tests_properties(openssh_container_integration PROPERTIES
      ENVIRONMENT "PYTHONPATH=${CMAKE_CURRENT_SOURCE_DIR}/../agent/src"
      TIMEOUT 180
    )
  endif()
endif()

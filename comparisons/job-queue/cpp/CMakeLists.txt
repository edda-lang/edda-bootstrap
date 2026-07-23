cmake_minimum_required(VERSION 3.20)
project(jobqueue CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

if(NOT CMAKE_BUILD_TYPE AND NOT CMAKE_CONFIGURATION_TYPES)
    set(CMAKE_BUILD_TYPE Release CACHE STRING "" FORCE)
endif()

if(MSVC)
    add_compile_options(/W4 /permissive- /Zc:__cplusplus)
else()
    add_compile_options(-Wall -Wextra -Wpedantic)
endif()

include(FetchContent)
set(FETCHCONTENT_QUIET OFF)

add_library(jobqueue STATIC
    src/clock.cpp
    src/storage.cpp
    src/jobqueue.cpp
    src/cascade.cpp
    src/admin.cpp
)
target_include_directories(jobqueue PUBLIC include)
target_compile_features(jobqueue PUBLIC cxx_std_20)

if(NOT WIN32)
    find_package(Threads REQUIRED)
    target_link_libraries(jobqueue PUBLIC Threads::Threads)
endif()

option(JOBQUEUE_BUILD_TESTS "Build jobqueue tests" ON)
if(JOBQUEUE_BUILD_TESTS)
    enable_testing()

    FetchContent_Declare(
        Catch2
        GIT_REPOSITORY https://github.com/catchorg/Catch2.git
        GIT_TAG v3.5.4
        GIT_SHALLOW TRUE
    )
    FetchContent_MakeAvailable(Catch2)

    add_executable(jobqueue_tests
        tests/test_basics.cpp
        tests/test_lease.cpp
        tests/test_retry.cpp
        tests/test_persistence.cpp
        tests/test_concurrency.cpp
        tests/test_capacity.cpp
        tests/test_time.cpp
        tests/test_priority.cpp
        tests/test_scheduled.cpp
        tests/test_metrics.cpp
        tests/test_interaction.cpp
        tests/test_dependencies.cpp
        tests/test_workers.cpp
        tests/test_cancellation.cpp
        tests/test_audit.cpp
        tests/test_namespaces.cpp
        tests/test_promotion.cpp
        tests/test_crosscut.cpp
    )
    target_link_libraries(jobqueue_tests PRIVATE jobqueue Catch2::Catch2WithMain)
    target_compile_features(jobqueue_tests PRIVATE cxx_std_20)

    include(CTest)
    include(Catch)
    catch_discover_tests(jobqueue_tests)
endif()

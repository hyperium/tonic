cmake_minimum_required(VERSION 3.14)
project(protoc-gen-rust-grpc LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

# Options
set(PROTOBUF_VERSION "32.0" CACHE STRING "Version of protobuf to download")
option(BUILD_SHARED_LIBS "Build shared libraries" OFF)

# Add cmake directory to module path
list(APPEND CMAKE_MODULE_PATH "${CMAKE_CURRENT_SOURCE_DIR}/cmake")

# Include modules
include(FetchContent)
include(FetchProtobuf)

# Download and configure protobuf
fetch_protobuf(${PROTOBUF_VERSION})

# Download and configure abseil
# Abseil version that's compatible with protobuf
set(ABSEIL_VERSION "20240722.0")
set(ABSEIL_URL "https://github.com/abseil/abseil-cpp/archive/refs/tags/${ABSEIL_VERSION}.tar.gz")

message(STATUS "Downloading abseil-cpp ${ABSEIL_VERSION}")

FetchContent_Declare(
    absl
    URL ${ABSEIL_URL}
    URL_HASH SHA256=f50e5ac311a81382da7fa75b97310e4b9006474f9560ac46f54a9967f07d4ae3
    DOWNLOAD_EXTRACT_TIMESTAMP TRUE
)

# Set abseil build options
set(ABSL_PROPAGATE_CXX_STD ON CACHE BOOL "" FORCE)
set(ABSL_BUILD_TESTING OFF CACHE BOOL "" FORCE)
set(BUILD_TESTING OFF CACHE BOOL "" FORCE)

# Force using the fetched abseil instead of system abseil
set(CMAKE_FIND_PACKAGE_PREFER_CONFIG TRUE)
set(absl_DIR "${CMAKE_CURRENT_BINARY_DIR}/_deps/absl-build" CACHE PATH "" FORCE)
# Prevent finding system libraries
set(CMAKE_PREFIX_PATH "" CACHE STRING "" FORCE)

FetchContent_MakeAvailable(absl)

# Add the protoc-gen-rust-grpc executable
add_executable(protoc-gen-rust-grpc
    src/grpc_rust_plugin.cc
    src/grpc_rust_generator.cc
    src/grpc_rust_generator.h
)

# Link against protobuf and abseil
target_link_libraries(protoc-gen-rust-grpc
    PRIVATE
        protobuf::libprotoc
        protobuf::libprotobuf
        absl::flat_hash_map
        absl::strings
        absl::string_view
)

# Include directories
target_include_directories(protoc-gen-rust-grpc
    PRIVATE
        ${protobuf_SOURCE_DIR}/src
        ${CMAKE_CURRENT_SOURCE_DIR}
)

# Set output directory
set_target_properties(protoc-gen-rust-grpc
    PROPERTIES
        RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin
)

# Add protoc executable as a custom target that copies the built protoc
# We explicitly name it "protoc" without version suffix
if(WIN32)
    set(PROTOC_OUTPUT_NAME "protoc.exe")
else()
    set(PROTOC_OUTPUT_NAME "protoc")
endif()

add_custom_target(copy-protoc ALL
    COMMAND ${CMAKE_COMMAND} -E copy_if_different
        $<TARGET_FILE:protoc>
        ${CMAKE_BINARY_DIR}/bin/${PROTOC_OUTPUT_NAME}
    DEPENDS protoc
    COMMENT "Copying protoc to output directory"
)

# Installation rules
install(TARGETS protoc-gen-rust-grpc
    RUNTIME DESTINATION bin
)
install(PROGRAMS ${CMAKE_BINARY_DIR}/bin/${PROTOC_OUTPUT_NAME}
    DESTINATION bin
)

# Print summary
message(STATUS "")
message(STATUS "protoc-gen-rust-grpc configuration summary:")
message(STATUS "  Protobuf version: ${PROTOBUF_VERSION}")
message(STATUS "  Abseil version: ${ABSEIL_VERSION}")
message(STATUS "  Build type: ${CMAKE_BUILD_TYPE}")
message(STATUS "  C++ standard: ${CMAKE_CXX_STANDARD}")
message(STATUS "  Output directory: ${CMAKE_BINARY_DIR}/bin")
message(STATUS "  Binaries to build: protoc, protoc-gen-rust-grpc")

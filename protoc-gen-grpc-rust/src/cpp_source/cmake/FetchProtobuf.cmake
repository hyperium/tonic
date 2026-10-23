# FetchProtobuf.cmake - Helper to download and configure protobuf
#
# This file provides a function to download protobuf from GitHub releases
# with automatic hash verification.

function(fetch_protobuf VERSION)
    include(FetchContent)

    # Map of known protobuf versions to their SHA256 hashes
    # You can add more versions here as needed
    if(VERSION STREQUAL "35.1")
        set(HASH "f0b6838e7522a8da96126d487068c959bc624926368f3024ac8fd03abd0a1ac4")
    elseif(VERSION STREQUAL "34.0")
        set(HASH "e540aae70d3b4f758846588768c9e39090fab880bc3233a1f42a8ab8d3781efd")
    elseif(VERSION STREQUAL "33.0")
        set(HASH "cbc536064706b628dcfe507bef386ef3e2214d563657612296f1781aa155ee07")
    else()
        message(FATAL_ERROR "Unknown protobuf version ${VERSION}; cannot download")
    endif()

    set(PROTOBUF_URL "https://github.com/protocolbuffers/protobuf/releases/download/v${VERSION}/protobuf-${VERSION}.tar.gz")

    message(STATUS "Fetching protobuf ${VERSION} from ${PROTOBUF_URL}")

    FetchContent_Declare(
        protobuf
        URL ${PROTOBUF_URL}
        URL_HASH SHA256=${HASH}
        DOWNLOAD_EXTRACT_TIMESTAMP TRUE
    )

    # Set protobuf build options before FetchContent_MakeAvailable
    set(protobuf_BUILD_TESTS OFF CACHE BOOL "" FORCE)
    set(protobuf_BUILD_CONFORMANCE OFF CACHE BOOL "" FORCE)
    set(protobuf_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
    set(protobuf_BUILD_PROTOC_BINARIES ON CACHE BOOL "" FORCE)
    set(protobuf_BUILD_SHARED_LIBS ${BUILD_SHARED_LIBS} CACHE BOOL "" FORCE)
    set(protobuf_INSTALL OFF CACHE BOOL "" FORCE)
    set(protobuf_WITH_ZLIB OFF CACHE BOOL "" FORCE)
    set(protobuf_MSVC_STATIC_RUNTIME OFF CACHE BOOL "" FORCE)

    FetchContent_MakeAvailable(protobuf)

    # Export the source directory for later use
    set(protobuf_SOURCE_DIR ${protobuf_SOURCE_DIR} PARENT_SCOPE)
endfunction()

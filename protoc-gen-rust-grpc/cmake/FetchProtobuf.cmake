# FetchProtobuf.cmake - Helper to download and configure protobuf
#
# This file provides a function to download protobuf from GitHub releases
# with automatic hash verification.

function(fetch_protobuf VERSION)
    include(FetchContent)

    # Map of known protobuf versions to their SHA256 hashes
    # You can add more versions here as needed
    if(VERSION STREQUAL "33.0")
        set(HASH "cbc536064706b628dcfe507bef386ef3e2214d563657612296f1781aa155ee07")
    elseif(VERSION STREQUAL "32.0")
        set(HASH "9dfdf08129f025a6c5802613b8ee1395044fecb71d38210ca59ecad283ef68bb")
    elseif(VERSION STREQUAL "31.1")
        set(HASH "12bfd76d27b9ac3d65c00966901609e020481b9474ef75c7ff4601ac06fa0b82")
    elseif(VERSION STREQUAL "28.3")
        set(HASH "35224c34cdc65a0b59938f62aebdc99c6285fc67f5c0ba5e8273b66179e1c106")
    elseif(VERSION STREQUAL "27.5")
        set(HASH "5c56c6be6ba37b0551f9c7b69e4e80d3df30bd962aacaed5ebf3e4df4bb0f746")
    else()
        message(WARNING "Unknown protobuf version ${VERSION}, downloading without hash verification")
        set(HASH "")
    endif()

    set(PROTOBUF_URL "https://github.com/protocolbuffers/protobuf/releases/download/v${VERSION}/protobuf-${VERSION}.tar.gz")

    message(STATUS "Fetching protobuf ${VERSION} from ${PROTOBUF_URL}")

    if(HASH)
        FetchContent_Declare(
            protobuf
            URL ${PROTOBUF_URL}
            URL_HASH SHA256=${HASH}
            DOWNLOAD_EXTRACT_TIMESTAMP TRUE
        )
    else()
        FetchContent_Declare(
            protobuf
            URL ${PROTOBUF_URL}
            DOWNLOAD_EXTRACT_TIMESTAMP TRUE
        )
    endif()

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

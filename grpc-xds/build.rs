/*
 *
 * Copyright 2025 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

//! Build-time xDS codegen.
//!
//! Regenerates the protobuf message modules from the vendored protos under
//! `proto/` into `OUT_DIR`, one protoc invocation per proto package, then lays
//! out a module tree that `src/lib.rs` includes. See `src/lib.rs` for how the
//! generated root module is pulled in.
//!
//! Grouping by package is required by the protobuf-rust generator: within a
//! single invocation every file is flattened into one namespace, and top-level
//! names are only guaranteed unique *within a package*. Files in OTHER packages
//! are declared as dependencies mapped to their in-crate module
//! (`crate::generated::<pkg>`), so cross-package references resolve there
//! instead of colliding.

use std::path::Path;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let gen_dir = out_dir.join("generated");

    let third_party = manifest.join("proto/third_party");
    println!("cargo:rerun-if-changed={}", third_party.display());

    // The five vendored dependency roots double as protoc include paths.
    let include_dirs: Vec<PathBuf> = [
        "envoy",
        "xds",
        "protoc-gen-validate",
        "googleapis",
        "cel-spec",
    ]
    .iter()
    .map(|d| third_party.join(d))
    .collect();

    // Resolve protoc. Locally, `protoc-gen-grpc-rust` builds it and we use that;
    // in CI its C++ build is skipped (`PROTOC_GEN_GRPC_RUST_NO_BUILD=1`) and a
    // prebuilt protoc is provided on `$PATH`, so `find_protoc` falls back to it.
    let protoc = find_protoc();

    // Collect every vendored proto as an include-relative import path, e.g.
    // "envoy/config/route/v3/route.proto".
    let mut protos: Vec<String> = Vec::new();

    // Generate `descriptor.proto` (from protoc's bundled include) as an
    // in-crate package `crate::generated::google::protobuf`. It's imported by
    // the option-defining protos to DEFINE custom options; generating it
    // in-crate means their reflection metadata (`__unstable`) can reference it
    // here instead of a symbol that doesn't exist in the external
    // well-known-types crate. protoc finds it via the bundled include path.
    //
    // We put it first in the protos list, so protoc places generated.rs in a
    // predictable location.
    protos.push("google/protobuf/descriptor.proto".to_string());

    for dir in &include_dirs {
        let mut found = Vec::new();
        collect_protos(dir, &mut found);
        for p in found {
            // Proto import paths are always forward-slash. Normalize here so
            // they match protoc's input/crate-mapping expectations and our
            // `/`-based package splitting on Windows, where `strip_prefix`
            // yields backslashes.
            let rel = p
                .strip_prefix(dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            protos.push(rel);
        }
    }

    // Fresh output tree.
    if gen_dir.exists() {
        std::fs::remove_dir_all(&gen_dir).unwrap();
    }
    std::fs::create_dir_all(&gen_dir).unwrap();

    protobuf_codegen::CodeGen::new()
        .protoc_path(&protoc)
        .inputs(protos.iter())
        .includes(include_dirs.iter())
        .output_dir(&gen_dir)
        .dependency(protobuf_well_known_types::get_dependency(
            "protobuf_well_known_types",
        ))
        .generate_and_compile()
        .unwrap_or_else(|e| panic!("xDS codegen failed: {e}"));
}

/// Resolves the `protoc` binary. Prefers the one built by `protoc-gen-grpc-rust`
/// (local dev); when that build is skipped (`PROTOC_GEN_GRPC_RUST_NO_BUILD`, as
/// in CI), falls back to `$PROTOC` or the first `protoc` on `$PATH`. Returns the
/// full path so the sibling `../include` dir can be located.
fn find_protoc() -> PathBuf {
    const PROTOC_NAME: &str = "protoc";
    // Prefer to use the protoc built by `protoc-gen-grpc-rust`.
    let built = protoc_gen_grpc_rust::protoc();
    if built.is_file() {
        return built;
    }
    // Fall back to `$PROTOC` if set.
    if let Some(p) = std::env::var_os("PROTOC").map(PathBuf::from)
        && p.is_file()
    {
        return p;
    }
    // Fall back to $PATH resolution.
    PROTOC_NAME.into()
}

/// Recursively collects `*.proto` file paths under `dir` into `out`.
fn collect_protos(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_protos(&path, out);
        } else if path.extension().is_some_and(|e| e == "proto") {
            out.push(path);
        }
    }
}

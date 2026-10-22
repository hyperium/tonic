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

use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

fn main() {
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

    // Resolve protoc. Locally, `protoc-gen-rust-grpc` builds it and we use that;
    // in CI its C++ build is skipped (`PROTOC_GEN_RUST_GRPC_NO_BUILD=1`) and a
    // prebuilt protoc is provided on `$PATH`, so `find_protoc` falls back to it.
    let protoc = find_protoc();

    // Collect every vendored proto as an include-relative import path, e.g.
    // "envoy/config/route/v3/route.proto".
    let mut protos: Vec<String> = Vec::new();
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
    // Also generate `descriptor.proto` (from protoc's bundled include) as an
    // in-crate package `crate::generated::google::protobuf`. It's imported by
    // the option-defining protos to DEFINE custom options; generating it
    // in-crate means their reflection metadata (`__unstable`) can reference it
    // here instead of a symbol that doesn't exist in the external
    // well-known-types crate. protoc finds it via the bundled include path.
    protos.push("google/protobuf/descriptor.proto".to_string());

    // Fresh output tree.
    if gen_dir.exists() {
        std::fs::remove_dir_all(&gen_dir).unwrap();
    }
    std::fs::create_dir_all(&gen_dir).unwrap();

    // Deps shared by every invocation: the well-known types resolve to the
    // external `protobuf_well_known_types` crate. (`descriptor.proto` is not
    // listed here — it's generated in-crate as its own package, so the
    // per-package loop below maps it to `crate::generated::google::protobuf`
    // like any other vendored proto.)
    let base_deps = protobuf_well_known_types::get_dependency("protobuf_well_known_types");

    // Group protos by package directory (each leaf dir is exactly one package)
    // and run one invocation per package.
    let mut packages: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for file in &protos {
        packages
            .entry(package_dir(file))
            .or_default()
            .push(file.clone());
    }

    for files in packages.values() {
        let mut deps = base_deps.clone();
        for other in &protos {
            if !files.contains(other) {
                deps.push(protobuf_codegen::Dependency {
                    crate_name: package_module_path(other),
                    proto_import_paths: Vec::new(),
                    proto_files: vec![other.clone()],
                });
            }
        }
        protobuf_codegen::CodeGen::new()
            .protoc_path(&protoc)
            .inputs(files.iter())
            .includes(include_dirs.iter())
            .output_dir(&gen_dir)
            .dependency(deps)
            .generate_and_compile()
            .unwrap_or_else(|e| {
                panic!(
                    "xDS codegen failed for package {}: {e}",
                    package_dir(&files[0])
                )
            });
    }

    // protobuf_codegen rewrites `crate_mapping.txt` in the output dir each
    // invocation; drop the leftover. The per-package `generated.rs` entry points
    // are kept — `write_module_tree` folds each into its package `mod.rs`.
    let _ = std::fs::remove_file(gen_dir.join("crate_mapping.txt"));

    write_module_tree(&gen_dir);
}

/// Returns the package directory of an include-relative proto path, e.g.
/// `envoy/config/route/v3/route.proto` -> `envoy/config/route/v3`.
fn package_dir(proto_rel: &str) -> &str {
    proto_rel
        .rsplit_once('/')
        .map(|(dir, _)| dir)
        .unwrap_or(proto_rel)
}

/// Maps an include-relative proto path to its package's in-crate module path,
/// e.g. `envoy/config/route/v3/route.proto` ->
/// `crate::generated::envoy::config::route::v3`. Every file in a package is
/// co-generated and flat-re-exported at this module, so a message `Foo` from any
/// file in the package is reachable as `<this path>::Foo`.
fn package_module_path(proto_rel: &str) -> String {
    let path: Vec<String> = package_dir(proto_rel).split('/').map(raw_ident).collect();
    format!("crate::generated::{}", path.join("::"))
}

/// Wraps a path segment as a raw identifier when it is a Rust keyword.
fn raw_ident(seg: &str) -> String {
    const KEYWORDS: &[&str] = &[
        "as", "break", "const", "continue", "else", "enum", "extern", "false", "fn", "for", "if",
        "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
        "static", "struct", "trait", "true", "type", "unsafe", "use", "where", "while", "async",
        "await", "dyn", "abstract", "become", "box", "do", "final", "macro", "override", "priv",
        "typeof", "unsized", "virtual", "yield", "try", "gen",
    ];
    if KEYWORDS.contains(&seg) {
        format!("r#{seg}")
    } else {
        seg.to_string()
    }
}

/// Builds the module tree under `dir`: each package directory's protoc entry
/// point (`generated.rs`) becomes that directory's `mod.rs` (re-exporting the
/// package's messages), and every directory declares `pub mod` for each of its
/// subdirectories with an ABSOLUTE `#[path]`. The root `mod.rs` is `include!`d
/// into `src/lib.rs` from `OUT_DIR`, so the generated tree is fully reachable as
/// `crate::generated::<pkg>::...`. For example,
/// `crate::generated::envoy::config::route::v3::RouteConfiguration`.
fn write_module_tree(dir: &Path) {
    const HEADER: &str = "// @generated at build time by grpc-xds/build.rs. Do not edit.";

    let mut subdirs: Vec<String> = Vec::new();
    let mut has_entry_point = false;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            subdirs.push(name);
            write_module_tree(&path);
        } else if name == "generated.rs" {
            has_entry_point = true;
        }
    }
    subdirs.sort();

    let mut m = format!("{HEADER}\n");

    // Fold the package's protoc entry point in as the module body: it declares
    // each `<file>.u.pb.rs` as a hidden submodule and flat-re-exports them, so
    // this module holds all of the package's messages (and intra-package
    // `super::…` refs resolve here). Its `#[path]`s are relative, which is fine
    // because this `mod.rs` is file-loaded (only the tree root is `include!`d).
    // The trailing `__unstable` reflection block is kept verbatim: its
    // cross-package `<pkg>::__unstable::<dep>` refs all resolve because every
    // dependency is either generated in-crate (including `descriptor.proto`) or
    // comes from `protobuf_well_known_types`.
    if has_entry_point {
        let entry = dir.join("generated.rs");
        let body = std::fs::read_to_string(&entry).unwrap();
        m.push_str(&body);
        if !m.ends_with('\n') {
            m.push('\n');
        }
        std::fs::remove_file(&entry).unwrap();
    }

    for s in &subdirs {
        let child = dir.join(s).join("mod.rs");
        m.push_str(&format!(
            "#[path = {:?}]\npub mod {};\n",
            child.to_string_lossy(),
            raw_ident(s)
        ));
    }

    std::fs::write(dir.join("mod.rs"), m).unwrap();
}

/// Resolves the `protoc` binary. Prefers the one built by `protoc-gen-rust-grpc`
/// (local dev); when that build is skipped (`PROTOC_GEN_RUST_GRPC_NO_BUILD`, as
/// in CI), falls back to `$PROTOC` or the first `protoc` on `$PATH`. Returns the
/// full path so the sibling `../include` dir can be located.
fn find_protoc() -> PathBuf {
    const PROTOC_NAME: &str = "protoc";
    // Prefer to use the protoc built by `protoc-gen-rust-grpc`.
    let built = protoc_gen_rust_grpc::protoc();
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

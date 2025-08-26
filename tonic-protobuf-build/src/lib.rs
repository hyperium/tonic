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

use std::fs::{self, read_to_string};
use std::io::Write;
use std::path::{Path, PathBuf};

use syn::parse_file;

pub fn protoc() -> String {
    format!("{}/bin/protoc", env!("OUT_DIR"))
}

pub fn protoc_gen_rust_grpc() -> String {
    format!("{}/bin/protoc-gen-rust-grpc", env!("OUT_DIR"))
}

pub fn bin() -> String {
    format!("{}/bin", env!("OUT_DIR"))
}

/// Details about a crate containing proto files with symbols referenced in
/// the file being compiled currently.
#[derive(Debug, Clone)]
pub struct Dependency {
    crate_name: String,
    proto_import_paths: Vec<PathBuf>,
    proto_files: Vec<String>,
}

impl Dependency {
    pub fn builder() -> DependencyBuilder {
        DependencyBuilder::default()
    }
}

#[derive(Default, Debug)]
pub struct DependencyBuilder {
    crate_name: Option<String>,
    proto_import_paths: Vec<PathBuf>,
    proto_files: Vec<String>,
}

impl DependencyBuilder {
    /// Name of the external crate.
    pub fn crate_name(mut self, name: impl Into<String>) -> Self {
        self.crate_name = Some(name.into());
        self
    }

    /// List of paths .proto files whose codegen is present in the crate. This
    /// is used to re-run the build command if required.
    pub fn proto_import_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.proto_import_paths.push(path.into());
        self
    }

    /// List of .proto file names whose codegen is present in the crate.
    pub fn proto_import_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.proto_import_paths = paths;
        self
    }

    pub fn proto_file(mut self, file: impl Into<String>) -> Self {
        self.proto_files.push(file.into());
        self
    }

    pub fn proto_files(mut self, files: Vec<String>) -> Self {
        self.proto_files = files;
        self
    }

    pub fn build(self) -> Result<Dependency, &'static str> {
        let crate_name = self.crate_name.ok_or("crate_name is required")?;
        Ok(Dependency {
            crate_name,
            proto_import_paths: self.proto_import_paths,
            proto_files: self.proto_files,
        })
    }
}

impl From<&Dependency> for protobuf_codegen::Dependency {
    fn from(val: &Dependency) -> Self {
        protobuf_codegen::Dependency {
            crate_name: val.crate_name.clone(),
            proto_import_paths: val.proto_import_paths.clone(),
            proto_files: val.proto_files.clone(),
        }
    }
}

/// Service generator builder.
#[derive(Debug, Clone)]
pub struct CodeGen {
    inputs: Vec<PathBuf>,
    output_dir: PathBuf,
    includes: Vec<PathBuf>,
    dependencies: Vec<Dependency>,
    message_module_path: Option<String>,
    // Whether to generate message code, defaults to true.
    generate_message_code: bool,
    should_format_code: bool,
}

impl CodeGen {
    pub fn new() -> Self {
        Self {
            inputs: Vec::new(),
            output_dir: PathBuf::from(std::env::var("OUT_DIR").unwrap()),
            includes: Vec::new(),
            dependencies: Vec::new(),
            message_module_path: None,
            generate_message_code: true,
            should_format_code: true,
        }
    }

    /// Sets whether to generate the message code. This can be disabled if the
    /// message code is being generated independently.
    pub fn generate_message_code(&mut self, enable: bool) -> &mut Self {
        self.generate_message_code = enable;
        self
    }

    /// Adds a proto file to compile.
    pub fn input(&mut self, input: impl AsRef<Path>) -> &mut Self {
        self.inputs.push(input.as_ref().to_owned());
        self
    }

    /// Adds a proto file to compile.
    pub fn inputs(&mut self, inputs: impl IntoIterator<Item = impl AsRef<Path>>) -> &mut Self {
        self.inputs
            .extend(inputs.into_iter().map(|input| input.as_ref().to_owned()));
        self
    }

    /// Enables or disables formatting of generated code.
    pub fn should_format_code(&mut self, enable: bool) -> &mut Self {
        self.should_format_code = enable;
        self
    }

    /// Sets the directory for the files generated by protoc. The generated code
    /// will be present in a subdirectory corresponding to the path of the
    /// proto file withing the included directories.
    pub fn output_dir(&mut self, output_dir: impl AsRef<Path>) -> &mut Self {
        self.output_dir = output_dir.as_ref().to_owned();
        self
    }

    /// Add a directory for protoc to scan for .proto files.
    pub fn include(&mut self, include: impl AsRef<Path>) -> &mut Self {
        self.includes.push(include.as_ref().to_owned());
        self
    }

    /// Add a directory for protoc to scan for .proto files.
    pub fn includes(&mut self, includes: impl Iterator<Item = impl AsRef<Path>>) -> &mut Self {
        self.includes.extend(
            includes
                .into_iter()
                .map(|include| include.as_ref().to_owned()),
        );
        self
    }

    /// Adds a list of Rust crates along with the proto files whose generated
    /// messages they contains.
    pub fn dependencies(&mut self, deps: Vec<Dependency>) -> &mut Self {
        self.dependencies.extend(deps);
        self
    }

    /// Sets path of the module containing the generated message code. This is
    /// "self" by default, i.e. the service code expects the message structs to
    /// be present in the same module. Set this if the message and service
    /// codegen needs to live in separate modules.
    pub fn message_module_path(&mut self, message_path: &str) -> &mut Self {
        self.message_module_path = Some(message_path.to_string());
        self
    }

    pub fn compile(&self) -> Result<(), String> {
        // Generate the message code.
        if self.generate_message_code {
            protobuf_codegen::CodeGen::new()
                .inputs(self.inputs.clone())
                .output_dir(self.output_dir.clone())
                .includes(self.includes.iter())
                .dependency(self.dependencies.iter().map(|d| d.into()).collect())
                .generate_and_compile()
                .unwrap();
        }
        let crate_mapping_path = if self.generate_message_code {
            self.output_dir.join("crate_mapping.txt")
        } else {
            self.generate_crate_mapping_file()
        };

        // Generate the service code.
        let mut cmd = std::process::Command::new("protoc");
        for input in &self.inputs {
            cmd.arg(input);
        }
        if !self.output_dir.exists() {
            // Attempt to make the directory if it doesn't exist
            let _ = std::fs::create_dir(&self.output_dir);
        }

        if !self.generate_message_code {
            for include in &self.includes {
                println!("cargo:rerun-if-changed={}", include.display());
            }
            for dep in &self.dependencies {
                for path in &dep.proto_import_paths {
                    println!("cargo:rerun-if-changed={}", path.display());
                }
            }
        }

        cmd.arg(format!("--rust-grpc_out={}", self.output_dir.display()));
        cmd.arg(format!(
            "--rust-grpc_opt=crate_mapping={}",
            crate_mapping_path.display()
        ));
        if let Some(message_path) = &self.message_module_path {
            cmd.arg(format!(
                "--rust-grpc_opt=message_module_path={message_path}",
            ));
        }

        for include in &self.includes {
            cmd.arg(format!("--proto_path={}", include.display()));
        }
        for dep in &self.dependencies {
            for path in &dep.proto_import_paths {
                cmd.arg(format!("--proto_path={}", path.display()));
            }
        }

        let output = cmd
            .output()
            .map_err(|e| format!("failed to run protoc: {e}"))?;
        println!("{}", std::str::from_utf8(&output.stdout).unwrap());
        eprintln!("{}", std::str::from_utf8(&output.stderr).unwrap());
        assert!(output.status.success());

        if self.should_format_code {
            self.format_code();
        }
        Ok(())
    }

    fn format_code(&self) {
        let mut generated_file_paths = Vec::new();
        let output_dir = &self.output_dir;
        if self.generate_message_code {
            generated_file_paths.push(output_dir.join("generated.rs"));
        }
        for proto_path in &self.inputs {
            let Some(stem) = proto_path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            generated_file_paths.push(output_dir.join(format!("{stem}_grpc.pb.rs")));
            if self.generate_message_code {
                generated_file_paths.push(output_dir.join(format!("{stem}.u.pb.rs")));
            }
        }

        for path in &generated_file_paths {
            // The path may not exist if there are no services present in the
            // proto file.
            if path.exists() {
                let src = read_to_string(path).expect("Failed to read generated file");
                let syntax = parse_file(&src).unwrap();
                let formatted = prettyplease::unparse(&syntax);
                fs::write(path, formatted).unwrap();
            }
        }
    }

    fn generate_crate_mapping_file(&self) -> PathBuf {
        let crate_mapping_path = self.output_dir.join("crate_mapping.txt");
        let mut file = fs::File::create(crate_mapping_path.clone()).unwrap();
        for dep in &self.dependencies {
            file.write_all(format!("{}\n", dep.crate_name).as_bytes())
                .unwrap();
            file.write_all(format!("{}\n", dep.proto_files.len()).as_bytes())
                .unwrap();
            for f in &dep.proto_files {
                file.write_all(format!("{f}\n").as_bytes()).unwrap();
            }
        }
        crate_mapping_path
    }
}

impl Default for CodeGen {
    fn default() -> Self {
        Self::new()
    }
}

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

#include "grpc_rust_generator.h"
#include <google/protobuf/compiler/code_generator.h>
#include <google/protobuf/compiler/plugin.h>
#include <google/protobuf/compiler/rust/context.h>
#include <google/protobuf/compiler/rust/crate_mapping.h>
#include <google/protobuf/compiler/rust/naming.h>
#include <vector>

namespace protobuf = google::protobuf;
namespace rust = google::protobuf::compiler::rust;

static std::string ReconstructParameterList(
    const std::vector<std::pair<std::string, std::string>> &options) {
  std::string result;
  for (const auto &[key, value] : options) {
    if (!result.empty()) {
      result += ",";
    }
    result += key + "=" + value;
  }
  return result;
}

class RustGrpcGenerator : public protobuf::compiler::CodeGenerator {
public:
  // Protobuf 5.27 released edition 2023.
#if GOOGLE_PROTOBUF_VERSION >= 5027000
  uint64_t GetSupportedFeatures() const override {
    return Feature::FEATURE_PROTO3_OPTIONAL |
           Feature::FEATURE_SUPPORTS_EDITIONS;
  }
  protobuf::Edition GetMinimumEdition() const override {
    return protobuf::Edition::EDITION_PROTO2;
  }
  protobuf::Edition GetMaximumEdition() const override {
    return protobuf::Edition::EDITION_2023;
  }
#else
  uint64_t GetSupportedFeatures() const override {
    return Feature::FEATURE_PROTO3_OPTIONAL;
  }
#endif

  bool Generate(const protobuf::FileDescriptor *file,
                const std::string &parameter,
                protobuf::compiler::GeneratorContext *context,
                std::string *error) const override {
    // Return early to avoid creating an empty output file.
    if (file->service_count() == 0) {
      return true;
    }
    std::vector<std::pair<std::string, std::string>> options;
    protobuf::compiler::ParseGeneratorParameter(parameter, &options);

    // Filter out GRPC options.
    std::vector<std::pair<std::string, std::string>> protobuf_options;
    rust_grpc_generator::GrpcOpts grpc_opts;
    for (auto opt : options) {
      if (opt.first == "message_module_path") {
        grpc_opts.message_module_path = opt.second;
      } else {
        protobuf_options.push_back(opt);
      }
    }

    if (grpc_opts.message_module_path.empty()) {
      grpc_opts.message_module_path = "self";
    }

    // The kernel isn't used by gRPC, it is there to pass Rust protobuf's
    // validation.
    protobuf_options.emplace_back("kernel", "upb");

    // Copied from protobuf rust's generator.cc.
    absl::StatusOr<rust::Options> opts =
        rust::Options::Parse(ReconstructParameterList(protobuf_options));
    if (!opts.ok()) {
      *error = std::string(opts.status().message());
      return false;
    }

    std::vector<const protobuf::FileDescriptor *> files_in_current_crate;
    context->ListParsedFiles(&files_in_current_crate);

    absl::StatusOr<absl::flat_hash_map<std::string, std::string>>
        import_path_to_crate_name = rust::GetImportPathToCrateNameMap(&*opts);
    if (!import_path_to_crate_name.ok()) {
      *error = std::string(import_path_to_crate_name.status().message());
      return false;
    }

    rust::RustGeneratorContext rust_generator_context(
        &files_in_current_crate, &*import_path_to_crate_name);

    rust::Context ctx_without_printer(&*opts, &rust_generator_context, nullptr,
                                      std::vector<std::string>());
    auto outfile = absl::WrapUnique(
        context->Open(rust_grpc_generator::GetRsGrpcFile(*file)));
    protobuf::io::Printer printer(outfile.get());
    rust::Context ctx = ctx_without_printer.WithPrinter(&printer);

    for (int i = 0; i < file->service_count(); ++i) {
      const protobuf::ServiceDescriptor *service = file->service(i);
      rust_grpc_generator::GenerateService(ctx, service, grpc_opts);
    }
    return true;
  }
};

int main(int argc, char *argv[]) {
  RustGrpcGenerator generator;
  return protobuf::compiler::PluginMain(argc, argv, &generator);
  return 0;
}

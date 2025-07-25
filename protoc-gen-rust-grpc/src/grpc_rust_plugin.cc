// Copyright 2025 gRPC authors.
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to
// deal in the Software without restriction, including without limitation the
// rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
// sell copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
// IN THE SOFTWARE.

#include <vector>

#include "google/protobuf/compiler/code_generator.h"
#include "google/protobuf/compiler/plugin.h"
#include "google/protobuf/io/printer.h"

#include "grpc_rust_generator.h"

namespace protobuf = google::protobuf;

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

    rust_grpc_generator::GrpcOpts grpc_opts;
    for (auto opt : options) {
      if (opt.first == "message_module_path") {
        grpc_opts.SetMessageModulePath(opt.second);
      } else if (opt.first == "crate_mapping") {
        absl::StatusOr<absl::flat_hash_map<std::string, std::string>>
            crate_map =
                rust_grpc_generator::GetImportPathToCrateNameMap(opt.second);
        if (crate_map.ok()) {
          grpc_opts.SetImportPathToCrateName(std::move(*crate_map));
        } else {
          *error = std::string(crate_map.status().message());
          return false;
        }
      }
    }

    std::vector<const google::protobuf::FileDescriptor *> files;
    context->ListParsedFiles(&files);
    grpc_opts.SetFilesInCurrentCrate(std::move(files));

    auto outfile = absl::WrapUnique(
        context->Open(rust_grpc_generator::GetRsGrpcFile(*file)));
    protobuf::io::Printer printer(outfile.get());

    for (int i = 0; i < file->service_count(); ++i) {
      const protobuf::ServiceDescriptor *service = file->service(i);
      rust_grpc_generator::GenerateService(printer, service, grpc_opts);
    }
    return true;
  }
};

int main(int argc, char *argv[]) {
  RustGrpcGenerator generator;
  return protobuf::compiler::PluginMain(argc, argv, &generator);
}

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

#include "src/grpc_rust_generator.h"

#include <string_view>
#include <vector>

#include "absl/strings/str_join.h"
#include "absl/strings/str_replace.h"
#include "absl/strings/str_split.h"
#include "absl/strings/string_view.h"
#include "google/protobuf/compiler/rust/naming.h"
#include "google/protobuf/descriptor.h"
#include "google/protobuf/descriptor.pb.h"

namespace rust_grpc_generator {
namespace protobuf = google::protobuf;
namespace rust = protobuf::compiler::rust;

using protobuf::Descriptor;
using protobuf::FileDescriptor;
using protobuf::MethodDescriptor;
using protobuf::ServiceDescriptor;
using protobuf::SourceLocation;
using protobuf::io::Printer;

namespace {
template <typename DescriptorType>
std::string GrpcGetCommentsForDescriptor(const DescriptorType *descriptor) {
  SourceLocation location;
  if (descriptor->GetSourceLocation(&location)) {
    return location.leading_comments.empty() ? location.trailing_comments
                                             : location.leading_comments;
  }
  return "";
}

std::string RustModuleForContainingType(const GrpcOpts &opts,
                                        const Descriptor *containing_type,
                                        const FileDescriptor &file) {
  std::vector<std::string> modules;
  // Innermost to outermost order.
  const Descriptor *parent = containing_type;
  while (parent != nullptr) {
    modules.push_back(rust::RsSafeName(rust::CamelToSnakeCase(parent->name())));
    parent = parent->containing_type();
  }

  // Reverse the vector to get submodules in outer-to-inner order).
  std::reverse(modules.begin(), modules.end());

  // If there are any modules at all, push an empty string on the end so that
  // we get the trailing ::
  if (!modules.empty()) {
    modules.push_back("");
  }

  std::string crate_relative = absl::StrJoin(modules, "::");

  if (opts.IsFileInCurrentCrate(file)) {
    return crate_relative;
  }
  std::string crate_name =
      absl::StrCat("::", rust::RsSafeName(opts.GetCrateName(file.name())));

  return absl::StrCat(crate_name, "::", crate_relative);
}

std::string RsTypePathWithinMessageModule(const GrpcOpts &opts,
                                          const Descriptor &msg) {
  return absl::StrCat(
      RustModuleForContainingType(opts, msg.containing_type(), *msg.file()),
      rust::RsSafeName(msg.name()));
}

std::string RsTypePath(const Descriptor &msg, const GrpcOpts &opts, int depth) {
  std::string path_within_module = RsTypePathWithinMessageModule(opts, msg);
  if (!opts.IsFileInCurrentCrate(*msg.file())) {
    return path_within_module;
  }
  std::string path_to_message_module = opts.GetMessageModulePath() + "::";
  if (path_to_message_module == "self::") {
    path_to_message_module = "";
  }

  // If the path to the message module is defined from the crate or global
  // root, we don't need to add a prefix of "super::"s.
  if (absl::StartsWith(path_to_message_module, "crate::") ||
      absl::StartsWith(path_to_message_module, "::")) {
    depth = 0;
  }
  std::string prefix = "";
  for (int i = 0; i < depth; ++i) {
    prefix += "super::";
  }
  return prefix + path_to_message_module + std::string(path_within_module);
}

absl::Status ReadFileToString(const absl::string_view name, std::string *output,
                              bool text_mode) {
  char buffer[1024];
  FILE *file = fopen(name.data(), text_mode ? "rt" : "rb");
  if (file == nullptr)
    return absl::NotFoundError("Could not open file");

  while (true) {
    size_t n = fread(buffer, 1, sizeof(buffer), file);
    if (n <= 0)
      break;
    output->append(buffer, n);
  }

  int error = ferror(file);
  if (fclose(file) != 0)
    return absl::InternalError("Failed to close file");
  if (error != 0) {
    return absl::ErrnoToStatus(error,
                               absl::StrCat("Failed to read the file ", name,
                                            ". Error code: ", error));
  }
  return absl::OkStatus();
}
} // namespace

absl::StatusOr<absl::flat_hash_map<std::string, std::string>>
GetImportPathToCrateNameMap(const absl::string_view mapping_file_path) {
  absl::flat_hash_map<std::string, std::string> mapping;
  std::string mapping_contents;
  absl::Status status =
      ReadFileToString(mapping_file_path, &mapping_contents, true);
  if (!status.ok()) {
    return status;
  }

  std::vector<absl::string_view> lines =
      absl::StrSplit(mapping_contents, '\n', absl::SkipEmpty());
  size_t len = lines.size();

  size_t idx = 0;
  while (idx < len) {
    absl::string_view crate_name = lines[idx++];
    size_t files_cnt;
    if (!absl::SimpleAtoi(lines[idx++], &files_cnt)) {
      return absl::InvalidArgumentError(
          "Couldn't parse number of import paths in mapping file");
    }
    for (size_t i = 0; i < files_cnt; ++i) {
      mapping.insert({std::string(lines[idx++]), std::string(crate_name)});
    }
  }
  return mapping;
}

// Method generation abstraction.
//
// Each service contains a set of generic methods that will be used by codegen
// to generate abstraction implementations for the provided methods.
class Method {
public:
  Method() = delete;

  explicit Method(const MethodDescriptor *method) : method_(method) {}

  // The name of the method in Rust style.
  std::string Name() const {
    return rust::RsSafeName(rust::CamelToSnakeCase(method_->name()));
  };

  // The fully-qualified name of the method, scope delimited by periods.
  absl::string_view FullName() const { return method_->full_name(); }

  // The name of the method as it appears in the .proto file.
  absl::string_view ProtoFieldName() const { return method_->name(); };

  // Checks if the method is streamed by the client.
  bool IsClientStreaming() const { return method_->client_streaming(); };

  // Checks if the method is streamed by the server.
  bool IsServerStreaming() const { return method_->server_streaming(); };

  // Get comments about this method.
  std::string Comment() const { return GrpcGetCommentsForDescriptor(method_); };

  // Checks if the method is deprecated. Default is false.
  bool IsDeprecated() const { return method_->options().deprecated(); }

  // Returns the Rust type name of request message.
  std::string RequestName(const GrpcOpts &opts, int depth) const {
    const Descriptor *input = method_->input_type();
    return RsTypePath(*input, opts, depth);
  };

  // Returns the Rust type name of response message.
  std::string ResponseName(const GrpcOpts &opts, int depth) const {
    const Descriptor *output = method_->output_type();
    return RsTypePath(*output, opts, depth);
  };

private:
  const MethodDescriptor *method_;
};

// Service generation abstraction.
//
// This class is an interface that can be implemented and consumed
// by client and server generators to allow any codegen module
// to generate service abstractions.
class Service {
public:
  Service() = delete;

  explicit Service(const ServiceDescriptor *service) : service_(service) {}

  // The name of the service, not including its containing scope.
  std::string Name() const {
    return rust::RsSafeName(rust::SnakeToUpperCamelCase(service_->name()));
  };

  // The fully-qualified name of the service, scope delimited by periods.
  absl::string_view FullName() const { return service_->full_name(); };

  // Returns a list of Methods provided by the service.
  std::vector<Method> Methods() const {
    std::vector<Method> ret;
    int methods_count = service_->method_count();
    ret.reserve(methods_count);
    for (int i = 0; i < methods_count; ++i) {
      ret.push_back(Method(service_->method(i)));
    }
    return ret;
  };

  // Get comments about this service.
  virtual std::string Comment() const {
    return GrpcGetCommentsForDescriptor(service_);
  };

private:
  const ServiceDescriptor *service_;
};

// Formats the full path for a method call. Returns the formatted method path
// (e.g., "/package.MyService/MyMethod")
static std::string FormatMethodPath(const Service &service,
                                    const Method &method) {
  return absl::StrFormat("/%s/%s", service.FullName(), method.ProtoFieldName());
}

static std::string SanitizeForRustDoc(absl::string_view raw_comment) {
  // 1. Escape the escape character itself first.
  std::string sanitized = absl::StrReplaceAll(raw_comment, {{"\\", "\\\\"}});

  // 2. Escape Markdown and Rustdoc special characters.
  sanitized = absl::StrReplaceAll(sanitized, {
                                                 {"`", "\\`"},
                                                 {"*", "\\*"},
                                                 {"_", "\\_"},
                                                 {"[", "\\["},
                                                 {"]", "\\]"},
                                                 {"#", "\\#"},
                                                 {"<", "\\<"},
                                                 {">", "\\>"},
                                             });

  return sanitized;
}

static std::string ProtoCommentToRustDoc(absl::string_view proto_comment) {
  std::string rust_doc;
  std::vector<std::string_view> lines = absl::StrSplit(proto_comment, '\n');
  for (const absl::string_view &line : lines) {
    // Preserve empty lines.
    if (line.empty()) {
      rust_doc += ("///\n");
    } else {
      rust_doc += absl::StrFormat("/// %s\n", SanitizeForRustDoc(line));
    }
  }
  return rust_doc;
}

static void GenerateDeprecated(Printer &ctx) { ctx.Emit("#[deprecated]\n"); }

namespace client {

static void GenerateMethods(Printer &printer, const Service &service,
                            const GrpcOpts &opts) {
  static const std::string unary_format = R"rs(
    pub async fn $ident$(
        &mut self,
        request: impl tonic::IntoRequest<$request$>,
    ) -> std::result::Result<tonic::Response<$response$>, tonic::Status> {
       self.inner.ready().await.map_err(|e| {
           tonic::Status::unknown(format!("Service was not ready: {}", e.into()))
       })?;
       let codec = $codec_name$::default();
       let path = http::uri::PathAndQuery::from_static("$path$");
       let mut req = request.into_request();
       req.extensions_mut().insert(GrpcMethod::new("$service_name$", "$method_name$"));
       self.inner.unary(req, path, codec).await
    }
    )rs";

  static const std::string server_streaming_format = R"rs(
        pub async fn $ident$(
            &mut self,
            request: impl tonic::IntoRequest<$request$>,
        ) -> std::result::Result<tonic::Response<tonic::codec::Streaming<$response$>>, tonic::Status> {
            self.inner.ready().await.map_err(|e| {
                tonic::Status::unknown(format!("Service was not ready: {}", e.into()))
            })?;
            let codec = $codec_name$::default();
            let path = http::uri::PathAndQuery::from_static("$path$");
            let mut req = request.into_request();
            req.extensions_mut().insert(GrpcMethod::new("$service_name$", "$method_name$"));
            self.inner.server_streaming(req, path, codec).await
        }
      )rs";

  static const std::string client_streaming_format = R"rs(
        pub async fn $ident$(
            &mut self,
            request: impl tonic::IntoStreamingRequest<Message = $request$>
        ) -> std::result::Result<tonic::Response<$response$>, tonic::Status> {
            self.inner.ready().await.map_err(|e| {
                tonic::Status::unknown(format!("Service was not ready: {}", e.into()))
            })?;
            let codec = $codec_name$::default();
            let path = http::uri::PathAndQuery::from_static("$path$");
            let mut req = request.into_streaming_request();
            req.extensions_mut().insert(GrpcMethod::new("$service_name$", "$method_name$"));
            self.inner.client_streaming(req, path, codec).await
        }
      )rs";

  static const std::string streaming_format = R"rs(
        pub async fn $ident$(
            &mut self,
            request: impl tonic::IntoStreamingRequest<Message = $request$>
        ) -> std::result::Result<tonic::Response<tonic::codec::Streaming<$response$>>, tonic::Status> {
            self.inner.ready().await.map_err(|e| {
                tonic::Status::unknown(format!("Service was not ready: {}", e.into()))
            })?;
            let codec = $codec_name$::default();
            let path = http::uri::PathAndQuery::from_static("$path$");
            let mut req = request.into_streaming_request();
            req.extensions_mut().insert(GrpcMethod::new("$service_name$", "$method_name$"));
            self.inner.streaming(req, path, codec).await
        }
      )rs";

  const std::vector<Method> methods = service.Methods();
  for (const Method &method : methods) {
    printer.Emit(ProtoCommentToRustDoc(method.Comment()));
    if (method.IsDeprecated()) {
      GenerateDeprecated(printer);
    }
    const std::string request_type = method.RequestName(opts, 1);
    const std::string response_type = method.ResponseName(opts, 1);
    {
      auto vars =
          printer.WithVars({{"codec_name", "tonic_protobuf::ProtoCodec"},
                            {"ident", method.Name()},
                            {"request", request_type},
                            {"response", response_type},
                            {"service_name", service.FullName()},
                            {"path", FormatMethodPath(service, method)},
                            {"method_name", method.ProtoFieldName()}});

      if (!method.IsClientStreaming() && !method.IsServerStreaming()) {
        printer.Emit(unary_format);
      } else if (!method.IsClientStreaming() && method.IsServerStreaming()) {
        printer.Emit(server_streaming_format);
      } else if (method.IsClientStreaming() && !method.IsServerStreaming()) {
        printer.Emit(client_streaming_format);
      } else {
        printer.Emit(streaming_format);
      }
      if (&method != &methods.back()) {
        printer.Emit("\n");
      }
    }
  }
}

static void GenerateClient(const Service &service, Printer &printer,
                           const GrpcOpts &opts) {
  std::string service_ident = absl::StrFormat("%sClient", service.Name());
  std::string client_mod =
      absl::StrFormat("%s_client", rust::CamelToSnakeCase(service.Name()));
  printer.Emit(
      {
          {"client_mod", client_mod},
          {"service_ident", service_ident},
          {"service_doc",
           [&] { printer.Emit(ProtoCommentToRustDoc(service.Comment())); }},
          {"methods", [&] { GenerateMethods(printer, service, opts); }},
      },
      R"rs(
      /// Generated client implementations.
      pub mod $client_mod$ {
          #![allow(
              unused_variables,
              dead_code,
              missing_docs,
              clippy::wildcard_imports,
              // will trigger if compression is disabled
              clippy::let_unit_value,
          )]
          use tonic::codegen::*;
          use tonic::codegen::http::Uri;

          $service_doc$
          #[derive(Debug, Clone)]
          pub struct $service_ident$<T> {
              inner: tonic::client::Grpc<T>,
          }

          impl<T> $service_ident$<T>
          where
              T: tonic::client::GrpcService<tonic::body::Body>,
              T::Error: Into<StdError>,
              T::ResponseBody: Body<Data = Bytes> + std::marker::Send  +
              'static, <T::ResponseBody as Body>::Error: Into<StdError> +
              std::marker::Send,
          {
              pub fn new(inner: T) -> Self {
                  let inner = tonic::client::Grpc::new(inner);
                  Self { inner }
              }

              pub fn with_origin(inner: T, origin: Uri) -> Self {
                  let inner = tonic::client::Grpc::with_origin(inner, origin);
                  Self { inner }
              }

              pub fn with_interceptor<F>(inner: T, interceptor: F) ->
              $service_ident$<InterceptedService<T, F>> where
                  F: tonic::service::Interceptor,
                  T::ResponseBody: Default,
                  T: tonic::codegen::Service<
                      http::Request<tonic::body::Body>,
                      Response = http::Response<<T as
                      tonic::client::GrpcService<tonic::body::Body>>::ResponseBody>
                  >,
                  <T as
                  tonic::codegen::Service<http::Request<tonic::body::Body>>>::Error:
                  Into<StdError> + std::marker::Send + std::marker::Sync,
              {
                  $service_ident$::new(InterceptedService::new(inner, interceptor))
              }

              /// Compress requests with the given encoding.
              ///
              /// This requires the server to support it otherwise it might respond with an
              /// error.
              #[must_use]
              pub fn send_compressed(mut self, encoding: CompressionEncoding)
              -> Self {
                  self.inner = self.inner.send_compressed(encoding);
                  self
              }

              /// Enable decompressing responses.
              #[must_use]
              pub fn accept_compressed(mut self, encoding:
              CompressionEncoding) -> Self {
                  self.inner = self.inner.accept_compressed(encoding);
                  self
              }

              /// Limits the maximum size of a decoded message.
              ///
              /// Default: `4MB`
              #[must_use]
              pub fn max_decoding_message_size(mut self, limit: usize) ->
              Self {
                  self.inner = self.inner.max_decoding_message_size(limit);
                  self
              }

              /// Limits the maximum size of an encoded message.
              ///
              /// Default: `usize::MAX`
              #[must_use]
              pub fn max_encoding_message_size(mut self, limit: usize) ->
              Self {
                  self.inner = self.inner.max_encoding_message_size(limit);
                  self
              }

              $methods$
          }
      })rs");
}

} // namespace client

void GenerateService(protobuf::io::Printer &printer,
                     const ServiceDescriptor *service_desc,
                     const GrpcOpts &opts) {
  client::GenerateClient(Service(service_desc), printer, opts);
}

std::string GetRsGrpcFile(const protobuf::FileDescriptor &file) {
  absl::string_view basename = absl::StripSuffix(file.name(), ".proto");
  return absl::StrCat(basename, "_grpc.pb.rs");
}

} // namespace rust_grpc_generator

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
  // Remove trailing empty lines.
  while (!lines.empty() && lines.back().empty()) {
    lines.pop_back();
  }
  for (const absl::string_view &line : lines) {
    // Preserve empty lines.
    if (line.empty()) {
      rust_doc += ("///\n");
    } else {
      rust_doc += absl::StrFormat("///%s\n", SanitizeForRustDoc(line));
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

namespace server {
static void GenerateTraitMethods(Printer &printer, const Service &service,
                                 const GrpcOpts &opts) {
  static const std::string unary_format = R"rs(
    $method_doc$
    async fn $name$(&self, request: tonic::Request<$request$>)
        -> std::result::Result<tonic::Response<$response$>, tonic::Status> {
        Err(tonic::Status::unimplemented("Not yet implemented"))
    }
  )rs";

  static const std::string client_streaming_format = R"rs(
    $method_doc$
    async fn $name$(&self, request: tonic::Request<tonic::Streaming<$request$>>)
        -> std::result::Result<tonic::Response<$response$>, tonic::Status> {
        Err(tonic::Status::unimplemented("Not yet implemented"))
    }
    )rs";

  static const std::string server_streaming_format = R"rs(
    $method_doc$
    async fn $name$(&self, request: tonic::Request<$request$>)
        -> std::result::Result<tonic::Response<BoxStream<$response$>>, tonic::Status> {
        Err(tonic::Status::unimplemented("Not yet implemented"))
    }
    )rs";

  static const std::string streaming_format = R"rs(
    $method_doc$
    async fn $name$(&self, request: tonic::Request<tonic::Streaming<$request$>>)
        -> std::result::Result<tonic::Response<BoxStream<$response$>>, tonic::Status> {
        Err(tonic::Status::unimplemented("Not yet implemented"))
    }
    )rs";

  const std::vector<Method> methods = service.Methods();
  for (const Method &method : methods) {
    const std::string request_type = method.RequestName(opts, 1);
    const std::string response_type = method.ResponseName(opts, 1);
    auto vars = printer.WithVars({
        {"name", method.Name()},
        {"request", request_type},
        {"response", response_type},
        {"method_doc", ProtoCommentToRustDoc(method.Comment())},
    });
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

static void GenerateTrait(Printer &printer, const Service &service,
                          const GrpcOpts &opts) {
  const std::string trait_doc = ProtoCommentToRustDoc(
      " Generated trait containing gRPC methods that should "
      "be implemented for use with " +
      service.Name() + "Server.");
  printer.Emit(
      {
          {"trait_doc", trait_doc},
          {"methods", [&] { GenerateTraitMethods(printer, service, opts); }},
      },
      R"rs(
    $trait_doc$
    #[async_trait]
    pub trait $server_trait$ : std::marker::Send + std::marker::Sync + 'static {
        $methods$
    }
    )rs");
}

static void GenerateMethods(Printer &printer, const Service &service,
                            const GrpcOpts &opts) {
  static const std::string unary_format = R"rs(
    #[allow(non_camel_case_types)]
    struct $service_ident$<T: $server_trait$ >(pub Arc<T>);

    impl<T: $server_trait$> tonic::server::UnaryService<$request$> for $service_ident$<T> {
        type Response = $response$;
        type Future = BoxFuture<tonic::Response<Self::Response>, tonic::Status>;

        fn call(&mut self, request: tonic::Request<$request$>) -> Self::Future {
            let inner = Arc::clone(&self.0);
            let fut = async move {
                <T as $server_trait$>::$method_ident$(&inner, request).await
            };
            Box::pin(fut)
        }
    }

    let accept_compression_encodings = self.accept_compression_encodings;
    let send_compression_encodings = self.send_compression_encodings;
    let max_decoding_message_size = self.max_decoding_message_size;
    let max_encoding_message_size = self.max_encoding_message_size;
    let inner = self.inner.clone();
    let fut = async move {
        let method = $service_ident$(inner);
        let codec = $codec_name$::default();

        let mut grpc = tonic::server::Grpc::new(codec)
            .apply_compression_config(accept_compression_encodings, send_compression_encodings)
            .apply_max_message_size_config(max_decoding_message_size, max_encoding_message_size);

        let res = grpc.unary(method, req).await;
        Ok(res)
    };

    Box::pin(fut)
    )rs";

  static const std::string server_streaming_format = R"rs(
    #[allow(non_camel_case_types)]
    struct $service_ident$<T: $server_trait$ >(pub Arc<T>);

    impl<T: $server_trait$> tonic::server::ServerStreamingService<$request$> for $service_ident$<T> {
        type Response = $response$;
        type ResponseStream = BoxStream<$response$>;
        type Future = BoxFuture<tonic::Response<Self::ResponseStream>, tonic::Status>;

        fn call(&mut self, request: tonic::Request<$request$>) -> Self::Future {
            let inner = Arc::clone(&self.0);
            let fut = async move {
                <T as $server_trait$>::$method_ident$(&inner, request).await
            };
            Box::pin(fut)
        }
    }

    let accept_compression_encodings = self.accept_compression_encodings;
    let send_compression_encodings = self.send_compression_encodings;
    let max_decoding_message_size = self.max_decoding_message_size;
    let max_encoding_message_size = self.max_encoding_message_size;
    let inner = self.inner.clone();
    let fut = async move {
        let method = $service_ident$(inner);
        let codec = $codec_name$::default();

        let mut grpc = tonic::server::Grpc::new(codec)
            .apply_compression_config(accept_compression_encodings, send_compression_encodings)
            .apply_max_message_size_config(max_decoding_message_size, max_encoding_message_size);

        let res = grpc.server_streaming(method, req).await;
        Ok(res)
    };

    Box::pin(fut)
    )rs";

  static const std::string client_streaming_format = R"rs(
    #[allow(non_camel_case_types)]
    struct $service_ident$<T: $server_trait$ >(pub Arc<T>);

    impl<T: $server_trait$> tonic::server::ClientStreamingService<$request$> for $service_ident$<T>
    {
        type Response = $response$;
        type Future = BoxFuture<tonic::Response<Self::Response>, tonic::Status>;

        fn call(&mut self, request: tonic::Request<tonic::Streaming<$request$>>) -> Self::Future {
            let inner = Arc::clone(&self.0);
            let fut = async move {
                <T as $server_trait$>::$method_ident$(&inner, request).await
            };
            Box::pin(fut)
        }
    }

    let accept_compression_encodings = self.accept_compression_encodings;
    let send_compression_encodings = self.send_compression_encodings;
    let max_decoding_message_size = self.max_decoding_message_size;
    let max_encoding_message_size = self.max_encoding_message_size;
    let inner = self.inner.clone();
    let fut = async move {
        let method = $service_ident$(inner);
        let codec = $codec_name$::default();

        let mut grpc = tonic::server::Grpc::new(codec)
            .apply_compression_config(accept_compression_encodings, send_compression_encodings)
            .apply_max_message_size_config(max_decoding_message_size, max_encoding_message_size);

        let res = grpc.client_streaming(method, req).await;
        Ok(res)
    };

    Box::pin(fut)
  )rs";

  static const std::string streaming_format = R"rs(
    #[allow(non_camel_case_types)]
    struct $service_ident$<T: $server_trait$>(pub Arc<T>);

    impl<T: $server_trait$> tonic::server::StreamingService<$request$> for $service_ident$<T>
    {
        type Response = $response$;
        type ResponseStream = BoxStream<$response$>;
        type Future = BoxFuture<tonic::Response<Self::ResponseStream>, tonic::Status>;

        fn call(&mut self, request: tonic::Request<tonic::Streaming<$request$>>) -> Self::Future {
            let inner = Arc::clone(&self.0);
            let fut = async move {
                <T as $server_trait$>::$method_ident$(&inner, request).await
            };
            Box::pin(fut)
        }
    }

    let accept_compression_encodings = self.accept_compression_encodings;
    let send_compression_encodings = self.send_compression_encodings;
    let max_decoding_message_size = self.max_decoding_message_size;
    let max_encoding_message_size = self.max_encoding_message_size;
    let inner = self.inner.clone();
    let fut = async move {
        let method = $service_ident$(inner);
        let codec = $codec_name$::default();

        let mut grpc = tonic::server::Grpc::new(codec)
            .apply_compression_config(accept_compression_encodings, send_compression_encodings)
            .apply_max_message_size_config(max_decoding_message_size, max_encoding_message_size);

        let res = grpc.streaming(method, req).await;
        Ok(res)
    };

    Box::pin(fut)
  )rs";

  const std::vector<Method> methods = service.Methods();
  for (const Method &method : methods) {
    const std::string request_type = method.RequestName(opts, 1);
    const std::string response_type = method.ResponseName(opts, 1);
    printer.Emit(
        {
            {"codec_name", "tonic_protobuf::ProtoCodec"},
            {"service_ident", method.Name() + "Svc"},
            {"method_ident", method.Name()},
            {"request", request_type},
            {"response", response_type},
            {"server_trait", service.Name()},
            {"path", FormatMethodPath(service, method)},
            {"method_body",
             [&]() {
               if (!method.IsClientStreaming() && !method.IsServerStreaming()) {
                 printer.Emit(unary_format);
               } else if (!method.IsClientStreaming() &&
                          method.IsServerStreaming()) {
                 printer.Emit(server_streaming_format);
               } else if (method.IsClientStreaming() &&
                          !method.IsServerStreaming()) {
                 printer.Emit(client_streaming_format);
               } else {
                 printer.Emit(streaming_format);
               }
             }},
        },
        R"rs(
    "$path$" => {
        $method_body$
    }
    )rs");
  }
}

static void GenerateServer(const Service &service, Printer &printer,
                           const GrpcOpts &opts) {

  std::string server_mod =
      absl::StrFormat("%s_server", rust::CamelToSnakeCase(service.Name()));
  printer.Emit(
      {
          {"server_mod", server_mod},
          {"service_doc", ProtoCommentToRustDoc(service.Comment())},
          {"server_service", service.Name() + "Server"},
          {"service_name", service.FullName()},
          {"server_trait", service.Name()},
          {"generated_trait", [&] { GenerateTrait(printer, service, opts); }},
          {"methods", [&] { GenerateMethods(printer, service, opts); }},
      },
      R"rs(
    /// Generated server implementations.
    pub mod $server_mod$ {
        #![allow(
            unused_variables,
            dead_code,
            missing_docs,
            clippy::wildcard_imports,
            // will trigger if compression is disabled
            clippy::let_unit_value,
        )]
        use tonic::codegen::*;

        $generated_trait$

        $service_doc$
        #[derive(Debug)]
        pub struct $server_service$<T> {
            inner: Arc<T>,
            accept_compression_encodings: EnabledCompressionEncodings,
            send_compression_encodings: EnabledCompressionEncodings,
            max_decoding_message_size: Option<usize>,
            max_encoding_message_size: Option<usize>,
        }

        impl<T> $server_service$<T> {
            pub fn new(inner: T) -> Self {
                Self::from_arc(Arc::new(inner))
            }

            pub fn from_arc(inner: Arc<T>) -> Self {
                Self {
                    inner,
                    accept_compression_encodings: Default::default(),
                    send_compression_encodings: Default::default(),
                    max_decoding_message_size: None,
                    max_encoding_message_size: None,
                }
            }

            pub fn with_interceptor<F>(inner: T, interceptor: F) -> InterceptedService<Self, F>
            where
                F: tonic::service::Interceptor,
            {
                InterceptedService::new(Self::new(inner), interceptor)
            }

            /// Enable decompressing requests with the given encoding.
            #[must_use]
            pub fn accept_compressed(mut self, encoding: CompressionEncoding) -> Self {
                self.accept_compression_encodings.enable(encoding);
                self
            }

            /// Compress responses with the given encoding, if the client supports it.
            #[must_use]
            pub fn send_compressed(mut self, encoding: CompressionEncoding) -> Self {
                self.send_compression_encodings.enable(encoding);
                self
            }

            /// Limits the maximum size of a decoded message.
            ///
            /// Default: `4MB`
            #[must_use]
            pub fn max_decoding_message_size(mut self, limit: usize) -> Self {
                self.max_decoding_message_size = Some(limit);
                self
            }

            /// Limits the maximum size of an encoded message.
            ///
            /// Default: `usize::MAX`
            #[must_use]
            pub fn max_encoding_message_size(mut self, limit: usize) -> Self {
                self.max_encoding_message_size = Some(limit);
                self
            }
        }

        impl<T, B> tonic::codegen::Service<http::Request<B>> for $server_service$<T>
            where
                T: $server_trait$,
                B: Body + std::marker::Send + 'static,
                B::Error: Into<StdError> + std::marker::Send + 'static,
        {
            type Response = http::Response<tonic::body::Body>;
            type Error = std::convert::Infallible;
            type Future = BoxFuture<Self::Response, Self::Error>;

            fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<std::result::Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }

            fn call(&mut self, req: http::Request<B>) -> Self::Future {
                match req.uri().path() {
                    $methods$

                    _ => Box::pin(async move {
                        let mut response = http::Response::new(tonic::body::Body::default());
                        let headers = response.headers_mut();
                        headers.insert(tonic::Status::GRPC_STATUS, (tonic::Code::Unimplemented as i32).into());
                        headers.insert(http::header::CONTENT_TYPE, tonic::metadata::GRPC_CONTENT_TYPE);
                        Ok(response)
                    }),
                }
            }
        }

        impl<T> Clone for $server_service$<T> {
            fn clone(&self) -> Self {
                let inner = self.inner.clone();
                Self {
                    inner,
                    accept_compression_encodings: self.accept_compression_encodings,
                    send_compression_encodings: self.send_compression_encodings,
                    max_decoding_message_size: self.max_decoding_message_size,
                    max_encoding_message_size: self.max_encoding_message_size,
                }
            }
        }

        /// Generated gRPC service name
        pub const SERVICE_NAME: &str = "$service_name$";

        impl<T> tonic::server::NamedService for $server_service$<T> {
            const NAME: &'static str = SERVICE_NAME;
        }
    }
  )rs");
}
} // namespace server

void GenerateService(protobuf::io::Printer &printer,
                     const ServiceDescriptor *service_desc,
                     const GrpcOpts &opts) {
  Service service = Service(service_desc);
  client::GenerateClient(service, printer, opts);
  printer.Print("\n");
  server::GenerateServer(service, printer, opts);
}

std::string GetRsGrpcFile(const protobuf::FileDescriptor &file) {
  absl::string_view basename = absl::StripSuffix(file.name(), ".proto");
  return absl::StrCat(basename, "_grpc.pb.rs");
}

} // namespace rust_grpc_generator

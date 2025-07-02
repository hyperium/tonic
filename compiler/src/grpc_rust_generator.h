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

#ifndef NET_GRPC_COMPILER_RUST_GENERATOR_H_
#define NET_GRPC_COMPILER_RUST_GENERATOR_H_

#include <stdlib.h> // for abort()

#include <google/protobuf/compiler/rust/context.h>
#include <google/protobuf/descriptor.h>
#include <google/protobuf/io/zero_copy_stream.h>

namespace rust_grpc_generator {

namespace impl {
namespace protobuf = google::protobuf;
} // namespace impl

class GrpcOpts {
  /// Path the module containing the generated message code. Defaults to
  /// "self", i.e. the message code and service code is present in the same
  /// module.
public:
  std::string message_module_path;
};

// Writes the generated service interface into the given ZeroCopyOutputStream
void GenerateService(
    impl::protobuf::compiler::rust::Context &rust_generator_context,
    const impl::protobuf::ServiceDescriptor *service, const GrpcOpts &opts);

std::string GetRsGrpcFile(const impl::protobuf::FileDescriptor &file);
} // namespace rust_grpc_generator

#endif // NET_GRPC_COMPILER_RUST_GENERATOR_H_

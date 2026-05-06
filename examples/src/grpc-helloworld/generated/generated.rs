#[path="helloworld.u.pb.rs"]
#[allow(nonstandard_style)]
pub mod internal_do_not_use_helloworld;

#[allow(unused_imports, nonstandard_style)]
pub use internal_do_not_use_helloworld::*;
pub mod __unstable {
pub static HELLOWORLD_DESCRIPTOR_INFO: ::protobuf::__internal::runtime::__unstable::DescriptorInfo = ::protobuf::__internal::runtime::__unstable::DescriptorInfo {
  descriptor: b"\n\x10helloworld.proto\x12\nhelloworld\"\x1c\n\x0cHelloRequest\x12\x0c\n\x04name\x18\x01 \x01(\t\"\x1d\n\nHelloReply\x12\x0f\n\x07message\x18\x01 \x01(\t2I\n\x07Greeter\x12>\n\x08SayHello\x12\x18.helloworld.HelloRequest\x1a\x16.helloworld.HelloReply\"\x00\x42\x30\n\x1bio.grpc.examples.helloworldB\x0fHelloWorldProtoP\x01\x62\x06proto3",
  deps: &[
  ],
};
}

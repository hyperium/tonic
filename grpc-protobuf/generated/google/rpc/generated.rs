#[path="status.u.pb.rs"]
#[allow(nonstandard_style, unused)]
pub mod google_rpc_status_proto;
#[allow(nonstandard_style, unused)]
#[doc(inline)]
pub use google_rpc_status_proto::*;
#[allow(nonstandard_style, unused)]
pub mod __unstable {
pub static GOOGLE_RPC_STATUS_DESCRIPTOR_INFO: ::protobuf::__internal::runtime::__unstable::DescriptorInfo = ::protobuf::__internal::runtime::__unstable::DescriptorInfo {
  descriptor: b"\n\x17google/rpc/status.proto\x12\ngoogle.rpc\x1a\x19google/protobuf/any.proto\"N\n\x06Status\x12\x0c\n\x04\x63ode\x18\x01 \x01(\x05\x12\x0f\n\x07message\x18\x02 \x01(\t\x12%\n\x07\x64\x65tails\x18\x03 \x03(\x0b\x32\x14.google.protobuf.AnyB^\n\x0e\x63om.google.rpcB\x0bStatusProtoP\x01Z7google.golang.org/genproto/googleapis/rpc/status;status\xa2\x02\x03RPCb\x06proto3",
  deps: &[
    &::protobuf_well_known_types::__unstable::GOOGLE_PROTOBUF_ANY_DESCRIPTOR_INFO,
  ],
};
}

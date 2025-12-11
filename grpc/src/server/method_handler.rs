mod bidi_streaming_adapter;
mod client_streaming_adapter;
mod server_streaming_adapter;

pub use bidi_streaming_adapter::BidiStreamingAdapter;
pub use client_streaming_adapter::ClientStreamingAdapter;

pub use server_streaming_adapter::ServerStreamingAdapter;

mod unary_adapter;
pub use unary_adapter::UnaryMethodAdapter;
mod generic_byte_stream_method_handler;
pub use generic_byte_stream_method_handler::GenericByteStreamMethodHandler;
mod codec_message_stream_handler;
pub use codec_message_stream_handler::CodecMessageStreamHandler;

mod message_stream_handler;
pub use message_stream_handler::MessageStreamHandler;

mod message_allocator;
pub use message_allocator::{
    HeapMessageAllocator, HeapMessageHolder, HeapRequestHolder, HeapResponseHolder,
    RpcMessageAllocator, RpcMessageHolder, RpcRequestHolder, RpcResponseHolder,
};

/// The default response body type produced by standard Codecs.
pub type CodecRespB = bytes::Bytes;

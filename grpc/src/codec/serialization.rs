pub mod deserialize;
#[cfg(all(feature = "prost", not(feature = "protobuf")))]
pub mod prost;
#[cfg(feature = "protobuf")]
pub mod protobuf;
pub mod serialize;

pub use deserialize::Deserialize;
pub use serialize::Serialize;

use bytes::{Buf as BytesBuf, BufMut as BytesBufMut};

/// gRPC's object-safe, drop-in version of `BufMut`
pub trait BufMut: BytesBufMut {
    /// A `dyn`-compatible alternative to `bytes::BufMut::put<T: Buf>`
    /// This allows implementations to optimize transfer ownership of
    /// pre-allocated buffers instead of having to copy them.
    /// The default implementation is in line with default non dyn
    /// Bufmut and performs copies.
    fn put_buf(&mut self, src: &mut dyn BytesBuf) {
        while src.has_remaining() {
            let s = src.chunk();
            let d = self.chunk_mut();
            let cnt = usize::min(s.len(), d.len());

            unsafe {
                std::ptr::copy_nonoverlapping(s.as_ptr(), d.as_mut_ptr(), cnt);
                self.advance_mut(cnt);
            }
            src.advance(cnt);
        }
    }
}

// Blanket implementation allows any standard type to upgrade into our BufMut.
impl<B: BytesBufMut> BufMut for B {}

/// gRPC's drop-in version of `Buf`
pub trait Buf: BytesBuf {}

impl<B: BytesBuf> Buf for B {}

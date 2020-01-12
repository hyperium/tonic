use bytes::{Buf, BufMut, Bytes, BytesMut};
use std::mem::MaybeUninit;

/// A buffer to decode messages from.
#[derive(Debug)]
pub struct DecodeBuf<'a> {
    buf: &'a mut BytesMut,
}

/// A buffer to encode a message into.
#[derive(Debug)]
pub struct EncodeBuf<'a> {
    buf: &'a mut BytesMut,
}

impl<'a> EncodeBuf<'a> {
    pub(crate) fn new(buf: &'a mut BytesMut) -> Self {
        EncodeBuf { buf }
    }
}

impl<'a> DecodeBuf<'a> {
    pub(crate) fn new(buf: &'a mut BytesMut) -> Self {
        DecodeBuf { buf }
    }
}

impl Buf for DecodeBuf<'_> {
    #[inline]
    fn remaining(&self) -> usize {
        self.buf.len()
    }

    #[inline]
    fn bytes(&self) -> &[u8] {
        self.buf.bytes()
    }

    #[inline]
    fn advance(&mut self, cnt: usize) {
        self.buf.advance(cnt)
    }

    fn to_bytes(&mut self) -> Bytes {
        self.buf.to_bytes()
    }
}

impl<'a> EncodeBuf<'a> {
    #[doc(hidden)]
    #[inline]
    pub fn reserve(&mut self, capacity: usize) {
        self.buf.reserve(capacity);
    }
}

impl<'a> BufMut for EncodeBuf<'a> {
    #[inline]
    fn remaining_mut(&self) -> usize {
        self.buf.remaining_mut()
    }

    #[inline]
    unsafe fn advance_mut(&mut self, cnt: usize) {
        self.buf.advance_mut(cnt)
    }

    #[inline]
    fn bytes_mut(&mut self) -> &mut [MaybeUninit<u8>] {
        self.buf.bytes_mut()
    }
}

use bytes::{Buf, Bytes, BytesMut};
use std::fmt;

/// A buffer to decode messages from.
pub struct DecodeBuf<'a> {
    buf: &'a mut BytesMut,
}

impl<'a> DecodeBuf<'a> {
    pub(crate) fn new(buf: &'a mut BytesMut) -> Self {
        DecodeBuf { buf }
    }
}

impl<'a> Buf for DecodeBuf<'a> {
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

impl<'a> fmt::Debug for DecodeBuf<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecodeBuf").finish()
    }
}
